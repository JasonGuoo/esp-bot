//! CUPS is the print queue authority. Submission and completion are distinct.
//! https://www.cups.org/doc/man-lp.html / https://www.cups.org/doc/spec-ipp.html
use std::{collections::BTreeMap,sync::Arc,time::Duration};
use serde_json::{json,Value};
use tokio::process::Command;
use super::{run_command,TaskContext,TaskRecord,TaskResult,TaskService,TaskState};

pub(super) async fn submit(context:TaskContext)->Result<TaskResult,String>{
    if !context.task.authorized_effect{return Err("print_requires_explicit_intent".into());}
    context.check_cancelled()?;
    if context.task.result["submission_started"]==true{
        reconcile_print(&context.service,&context.task).await?;
        let state=context.service.get(&context.task.task_id)?;
        return Ok(TaskResult{state:state.state,summary:state.result["summary"].as_str().unwrap_or("已核对已有打印请求。").into(),artifact_ids:Vec::new(),data:state.result});
    }
    let input=&context.task.input;
    let artifact=if let Some(id)=input["artifact_id"].as_str(){context.service.artifacts.verify(id)?}
    else if let Some(visual)=input.get("visual"){
        context.progress("preparing_pdf",json!({"adapter":"pillow"}))?;
        let prepared=super::prepare_local(&context.service,json!({"operation":"render","visual":visual,"options":{"print_pdf":true}}),Some(&context)).await?;
        let assets=super::import_outputs(&context,&prepared)?;
        assets.into_iter().find(|a|a.mime_type=="application/pdf").ok_or("print_pdf_missing")?
    }else{return Ok(needs_input("请先选择要打印的内容。",vec!["artifact_id或visual"]));};
    if artifact.mime_type!="application/pdf"{return Err("print_requires_pdf_artifact".into());}
    if !context.service.artifacts.read(&artifact.artifact_id,0,8)?.starts_with(b"%PDF-"){return Err("print_pdf_header_invalid".into());}
    context.service.checkpoint(&context.task.task_id,json!({"artifact_id":artifact.artifact_id,"artifact_digest":artifact.digest}))?;
    let printer=if let Some(printer)=input["printer"].as_str().or(context.service.config.default_printer.as_deref()){
        validate_printer(printer)?;printer.to_owned()
    }else{
        let mut command=Command::new("lpstat");command.args(["-d"]).env("LC_ALL","C");
        let output=run_command(command,None,Duration::from_secs(5),Some(&context)).await?;
        let text=String::from_utf8_lossy(&output.stdout);
        match text.trim().strip_prefix("system default destination: "){
            Some(name) if output.success=>{validate_printer(name)?;name.to_owned()},
            _=>return Ok(needs_input("Mac 还没有默认打印机，请选择一台打印机。",vec!["printer"])),
        }
    };
    let copies=if input["copies"].is_null(){1}else{input["copies"].as_u64().ok_or("print_copies_invalid")?};if !(1..=20).contains(&copies){return Err("print_copies_invalid".into());}
    let paper=if input["paper"].is_null(){"a4"}else{input["paper"].as_str().ok_or("print_paper_invalid")?};if !matches!(paper,"a4"|"letter"|"a5"|"legal"){return Err("print_paper_invalid".into());}
    let sides=if input["sides"].is_null(){"one-sided"}else{input["sides"].as_str().ok_or("print_sides_invalid")?};if !matches!(sides,"one-sided"|"two-sided-long-edge"|"two-sided-short-edge"){return Err("print_sides_invalid".into());}
    let ranges=if input["page_ranges"].is_null(){None}else{Some(input["page_ranges"].as_str().ok_or("print_page_ranges_invalid")?)};
    if let Some(ranges)=ranges{if ranges.is_empty()||ranges.len()>80||ranges.split(',').any(|part|match part.split_once('-'){
        Some((start,end))=>start.parse::<u32>().ok().zip(end.parse::<u32>().ok()).is_none_or(|(a,b)|a==0||b<a||b>10_000),
        None=>part.parse::<u32>().ok().is_none_or(|page|page==0||page>10_000),
    }){return Err("print_page_ranges_invalid".into());}}
    // Querying the real destination catches missing/renamed printers before the
    // effect boundary. CUPS still decides whether submitted options are valid.
    let mut probe=Command::new("lpstat");probe.args(["-p",&printer]).env("LC_ALL","C");
    if !run_command(probe,None,Duration::from_secs(5),Some(&context)).await?.success{return Ok(needs_input("找不到所选打印机，请在 Mac 上检查打印机。",vec!["printer"]));}
    let path=context.service.artifacts.path(&artifact.artifact_id)?;let title=job_title(&context.task.task_id);
    context.check_cancelled()?;
    let checkpoint=json!({"artifact_id":artifact.artifact_id,"artifact_digest":artifact.digest,"printer":printer,"copies":copies,"paper":paper,"sides":sides,
        "page_ranges":ranges,"job_title":title,"submission_started":true,"started_at_ms":crate::time::now_ms()});
    // The persisted fence is deliberately before lp. A crash anywhere after it
    // recovers as unknown and searches CUPS; it can never silently resubmit.
    context.service.begin_effect(&context.task.task_id,checkpoint.clone())?;
    let mut command=Command::new("lp");command.arg("-d").arg(&printer).arg("-n").arg(copies.to_string()).arg("-t").arg(&title)
        .arg("-o").arg(format!("media={paper}")).arg("-o").arg(format!("sides={sides}")).env("LC_ALL","C");
    if let Some(ranges)=ranges{command.arg("-o").arg(format!("page-ranges={ranges}"));}
    command.arg("--").arg(path);
    let output=run_command(command,None,Duration::from_secs(20),Some(&context)).await;
    let output=match output{Ok(output)=>output,Err(error)=>{
        let latest=context.service.get(&context.task.task_id)?;let _=reconcile_print(&context.service,&latest).await;
        let latest=context.service.get(&context.task.task_id)?;
        if matches!(latest.state,TaskState::Succeeded|TaskState::Cancelled|TaskState::Failed){return Ok(TaskResult{state:latest.state,summary:latest.result["summary"].as_str().unwrap_or("已核对打印任务。").into(),artifact_ids:vec![artifact.artifact_id],data:latest.result});}
        return Err(error);
    }};
    let job_id=parse_request_id(&String::from_utf8_lossy(&output.stdout),&printer);
    let Some(job_id)=job_id else{
        // Even nonzero exit is not enough to prove that the server did not
        // accept the job. Preserve this exact attempt for reconciliation.
        let mut data=checkpoint;data["submission_response"]=json!(if output.success{"job_id_missing"}else{"cups_rejected_or_connection_lost"});
        context.service.checkpoint(&context.task.task_id,data)?;return Err("print_submission_unknown".into());
    };
    let mut data=context.service.get(&context.task.task_id)?.result;data["job_id"]=json!(job_id);data["submitted_at_ms"]=json!(crate::time::now_ms());
    context.service.checkpoint(&context.task.task_id,data)?;context.progress("submitted",json!({"job_id":job_id}))?;
    let until=tokio::time::Instant::now()+Duration::from_secs(context.service.config.print_monitor_seconds);
    loop{
        let task=context.service.get(&context.task.task_id)?;
        if context.is_cancelled(){cancel_print(&context.service,&task).await?;}
        else{let _=reconcile_print(&context.service,&task).await;}
        let current=context.service.get(&context.task.task_id)?;
        if matches!(current.state,TaskState::Succeeded|TaskState::Cancelled|TaskState::Failed)||tokio::time::Instant::now()>=until||context.is_cancelled(){
            return Ok(TaskResult{state:current.state,summary:current.result["summary"].as_str().unwrap_or("已提交到打印队列，尚未收到完成回执。").into(),artifact_ids:vec![artifact.artifact_id],data:current.result});
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
fn needs_input(summary:&str,fields:Vec<&str>)->TaskResult{TaskResult{state:TaskState::AwaitingInput,summary:summary.into(),artifact_ids:Vec::new(),data:json!({"missing_fields":fields})}}
fn validate_printer(printer:&str)->Result<(),String>{if printer.is_empty()||printer.len()>127||!printer.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'_'|b'-'|b'.')){Err("print_printer_invalid".into())}else{Ok(())}}
fn job_title(id:&str)->String{format!("xiaodai:{id}")}
fn parse_request_id(output:&str,printer:&str)->Option<String>{output.split_whitespace().find(|part|part.rsplit_once('-').is_some_and(|(p,n)|p==printer&&!n.is_empty()&&n.bytes().all(|b|b.is_ascii_digit()))).map(str::to_owned)}
fn numeric_id(job:&str)->Result<i32,String>{job.rsplit_once('-').and_then(|(_,n)|n.parse::<i32>().ok()).filter(|n|*n>0).ok_or_else(||"print_job_id_invalid".into())}
fn print_data(task:&TaskRecord)->Value{
    let mut value=task.result.clone();if !value.is_object(){value=json!({});}
    if value["job_id"].is_null(){if let Some(data)=task.result.get("data").and_then(Value::as_object){for(k,v)in data{value[k]=v.clone();}}}value
}

pub async fn cancel_print(service:&Arc<TaskService>,task:&TaskRecord)->Result<(),String>{
    let mut data=print_data(task);data["cancel_requested"]=json!(true);service.checkpoint(&task.task_id,data.clone())?;
    if let Some(job_id)=data["job_id"].as_str(){
        numeric_id(job_id)?;let printer=data["printer"].as_str().ok_or("print_printer_missing")?;validate_printer(printer)?;
        if !job_id.starts_with(&format!("{printer}-")){return Err("print_job_binding_invalid".into());}
        let mut command=Command::new("cancel");command.arg(job_id).env("LC_ALL","C");
        let output=run_command(command,None,Duration::from_secs(10),None).await;
        service.progress(&task.task_id,"cancel_sent",json!({"job_id":job_id,"command_accepted":output.as_ref().is_ok_and(|o|o.success)}))?;
        if output.is_err(){service.set_state(&task.task_id,TaskState::Unknown,"cancel_unconfirmed",Some(data))?;return Ok(());}
    }
    reconcile_print(service,&service.get(&task.task_id)?).await
}

pub async fn reconcile_print(service:&Arc<TaskService>,task:&TaskRecord)->Result<(),String>{
    let mut data=print_data(task);let printer=data["printer"].as_str().or(task.input["printer"].as_str()).or(service.config.default_printer.as_deref()).ok_or("print_printer_missing")?.to_owned();validate_printer(&printer)?;
    let job=if let Some(id)=data["job_id"].as_str(){
        let id=numeric_id(id)?;query_job(id).await
    }else{
        let title=data["job_title"].as_str().unwrap_or("").to_owned();
        if title!=job_title(&task.task_id){return Err("print_attempt_missing".into());}
        query_jobs(&printer).await.and_then(|jobs|{
            let matches=jobs.into_iter().filter(|job|job.get("job-name").and_then(Value::as_str)==Some(title.as_str())).collect::<Vec<_>>();
            if matches.len()==1{Ok(matches.into_iter().next().unwrap())}else if matches.len()>1{Err("print_multiple_matching_jobs".into())}else{Err("print_job_not_found".into())}
        })
    };
    match job{
        Ok(job)=>{
            let id=job.get("job-id").and_then(Value::as_i64).filter(|n|*n>0&&*n<=i32::MAX as i64).ok_or("print_receipt_invalid")?;
            data["job_id"]=json!(format!("{printer}-{id}"));data["printer"]=json!(printer);data["cups_receipt"]=json!(job);data["reconciled_at_ms"]=json!(crate::time::now_ms());
            let job_state=job.get("job-state").and_then(Value::as_i64).unwrap_or(0);
            if data["cancel_requested"]==true&&matches!(job_state,3..=6){
                let mut command=Command::new("cancel");command.arg(format!("{printer}-{id}")).env("LC_ALL","C");
                let accepted=run_command(command,None,Duration::from_secs(10),None).await.is_ok_and(|o|o.success);
                data["summary"]=json!("已请求取消，等待打印队列确认；已经打印的纸张无法撤回。");
                service.progress(&task.task_id,"cancel_sent",json!({"job_id":data["job_id"],"command_accepted":accepted}))?;
                return service.set_state(&task.task_id,TaskState::CancelRequested,"awaiting_cancel_receipt",Some(data));
            }
            let(state,stage,summary)=match job_state{
                3=>(TaskState::Partial,"queued","已提交到打印队列，正在等待打印机。"),
                4=>(TaskState::Partial,"held","打印任务被挂起，正在等待打印队列恢复。"),
                5=>(TaskState::Partial,"printing","打印机正在处理；尚未收到完成回执。"),
                6=>(TaskState::Partial,"stopped","打印任务暂停了，请检查打印机状态。"),
                7=>(TaskState::Cancelled,"cancelled","打印队列已确认取消；已经打印的纸张无法撤回。"),
                8=>(TaskState::Failed,"aborted","打印队列报告任务中止，请检查打印机。"),
                9=>(TaskState::Succeeded,"completed","打印队列报告任务完成，请检查打印机出纸。"),
                _=>(TaskState::Unknown,"reconcile_required","暂时无法确认打印状态。"),
            };data["summary"]=json!(summary);
            service.progress(&task.task_id,stage,json!({"job_id":data["job_id"],"cups_job_state":job_state}))?;
            service.set_state(&task.task_id,state,stage,Some(data))
        },
        Err(error)=>{
            // A retained lpstat queue entry proves existence, not success. A
            // missing or historical entry never proves paper was printed.
            let mut pending=false;
            if let Some(id)=data["job_id"].as_str(){numeric_id(id)?;let mut command=Command::new("lpstat");command.args(["-W","not-completed","-o",id]).env("LC_ALL","C");
                if let Ok(output)=run_command(command,None,Duration::from_secs(5),None).await{pending=output.success&&String::from_utf8_lossy(&output.stdout).lines().any(|line|line.split_whitespace().next()==Some(id));}}
            data["reconcile_error"]=json!(error);data["reconciled_at_ms"]=json!(crate::time::now_ms());data["summary"]=json!(if pending{"已提交到打印队列，尚未收到完成回执。"}else{"打印状态暂时无法确认；不会自动重打。"});
            service.set_state(&task.task_id,if pending{TaskState::Partial}else{TaskState::Unknown},if pending{"awaiting_receipt"}else{"reconcile_required"},Some(data))
        }
    }
}

type Job=BTreeMap<String,Value>;
async fn query_job(id:i32)->Result<Job,String>{
    let request=ipp_request(0x0009,vec![(0x45,"job-uri",format!("ipp://localhost/jobs/{id}").into_bytes())]);
    let jobs=ipp(request).await?;jobs.into_iter().next().ok_or_else(||"print_job_not_found".into())
}
async fn query_jobs(printer:&str)->Result<Vec<Job>,String>{
    ipp(ipp_request(0x000a,vec![(0x45,"printer-uri",format!("ipp://localhost/printers/{printer}").into_bytes()),(0x44,"which-jobs",b"all".to_vec()),(0x21,"limit",1000i32.to_be_bytes().to_vec())])).await
}
fn ipp_request(operation:u16,attributes:Vec<(u8,&str,Vec<u8>)>)->Vec<u8>{
    let mut bytes=vec![2,0];bytes.extend_from_slice(&operation.to_be_bytes());bytes.extend_from_slice(&1i32.to_be_bytes());bytes.push(1);
    ipp_attribute(&mut bytes,0x47,"attributes-charset",b"utf-8");ipp_attribute(&mut bytes,0x48,"attributes-natural-language",b"en");
    for(tag,name,value)in attributes{ipp_attribute(&mut bytes,tag,name,&value);}
    for(i,name)in ["job-id","job-name","job-state","job-state-reasons","job-printer-uri","job-impressions-completed"].iter().enumerate(){ipp_attribute(&mut bytes,0x44,if i==0{"requested-attributes"}else{""},name.as_bytes());}
    bytes.push(3);bytes
}
fn ipp_attribute(bytes:&mut Vec<u8>,tag:u8,name:&str,value:&[u8]){bytes.push(tag);bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());bytes.extend_from_slice(name.as_bytes());bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());bytes.extend_from_slice(value);}
async fn ipp(body:Vec<u8>)->Result<Vec<Job>,String>{
    let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none()).build().map_err(|_|"cups_client_unavailable")?;
    let mut response=client.post("http://127.0.0.1:631/").header("Content-Type","application/ipp").body(body).send().await.map_err(|_|"cups_ipp_unavailable")?;
    if !response.status().is_success(){return Err("cups_ipp_unavailable".into());}
    let mut bytes=Vec::new();while let Some(chunk)=response.chunk().await.map_err(|_|"cups_ipp_read_failed")?{if bytes.len()+chunk.len()>1_048_576{return Err("cups_ipp_limit".into());}bytes.extend_from_slice(&chunk);}
    parse_ipp(&bytes)
}
fn parse_ipp(bytes:&[u8])->Result<Vec<Job>,String>{
    if bytes.len()<9||u16::from_be_bytes([bytes[2],bytes[3]])>0x00ff{return Err("cups_ipp_status".into());}
    let mut position=8;let mut jobs=Vec::new();let mut current=Job::new();let mut in_job=false;let mut previous_name=String::new();
    while position<bytes.len(){let tag=bytes[position];position+=1;if tag<=0x0f{
        if in_job&&!current.is_empty(){jobs.push(std::mem::take(&mut current));}if jobs.len()>1000{return Err("cups_ipp_limit".into());}
        if tag==3{return Ok(jobs);}in_job=tag==2;previous_name.clear();continue;
    }
        let name=ipp_bytes(bytes,&mut position)?;let value=ipp_bytes(bytes,&mut position)?;
        if !name.is_empty(){previous_name=String::from_utf8(name.to_vec()).map_err(|_|"cups_ipp_encoding")?;}
        if !in_job{continue;}if previous_name.is_empty(){return Err("cups_ipp_attribute_invalid".into());}
        let parsed=if matches!(tag,0x21|0x23)&&value.len()==4{json!(i32::from_be_bytes([value[0],value[1],value[2],value[3]]))}
            else if tag==0x22&&value.len()==1{json!(value[0]!=0)}else{json!(String::from_utf8_lossy(value).chars().take(2048).collect::<String>())};
        if let Some(existing)=current.get_mut(&previous_name){if let Some(list)=existing.as_array_mut(){list.push(parsed);}else{*existing=json!([existing.clone(),parsed]);}}
        else{current.insert(previous_name.clone(),parsed);}
    }Err("cups_ipp_truncated".into())
}
fn ipp_bytes<'a>(bytes:&'a[u8],position:&mut usize)->Result<&'a[u8],String>{
    if *position+2>bytes.len(){return Err("cups_ipp_truncated".into());}let len=u16::from_be_bytes([bytes[*position],bytes[*position+1]]) as usize;*position+=2;
    if *position+len>bytes.len(){return Err("cups_ipp_truncated".into());}let value=&bytes[*position..*position+len];*position+=len;Ok(value)
}
