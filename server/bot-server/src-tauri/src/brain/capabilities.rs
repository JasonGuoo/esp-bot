//! Bounded adapters around supported Mac services. These do not run an agent.
use std::{path::{Path,PathBuf},process::Stdio,sync::Arc,time::Duration};
use serde_json::{json,Value};
use tokio::{io::{AsyncRead,AsyncReadExt,AsyncWriteExt},process::Command};
use super::{artifacts::Artifact,display,presentation::PresentationPlan,tasks::{TaskConfig,TaskContext,TaskRecord,TaskResult,TaskService,TaskState}};

#[path="capabilities_print.rs"] mod printing;
pub use printing::{cancel_print,reconcile_print};

pub fn availability(config:&TaskConfig)->Value{
    let script=config.media_script.as_ref().is_some_and(|p|p.is_file());
    json!({"presentation.render":{"script_present":script,"python_present":executable(&config.python),"pillow":"checked_on_use","stroke_data_present":config.stroke_data_path.as_ref().is_some_and(|p|p.exists())},
        "media.prepare":{"script_present":script,"ffmpeg_present":executable(&config.ffmpeg),"ffprobe_present":executable(&config.ffprobe),"remote_hosts":config.allowed_media_hosts},
        "print.submit":{"lp_present":executable("lp"),"lpstat_present":executable("lpstat"),"configured_printer":config.default_printer,"scheduler":"checked_on_use"}})
}
fn executable(program:&str)->bool{
    if program.contains(std::path::MAIN_SEPARATOR){return Path::new(program).is_file();}
    std::env::var_os("PATH").is_some_and(|paths|std::env::split_paths(&paths).any(|p|p.join(program).is_file()))
        ||matches!(program,"ffmpeg"|"ffprobe")&&["/opt/homebrew/bin","/usr/local/bin"].iter().any(|root|Path::new(root).join(program).is_file())
}

pub async fn execute(context:TaskContext)->Result<TaskResult,String>{
    match context.task.capability.as_str(){
        "presentation.render"=>render_task(context).await,
        "media.prepare"=>media_task(context).await,
        "print.submit"=>printing::submit(context).await,
        "agent.work"=>Err("agent_worker_not_registered".into()),
        _=>Err("task_capability_unsupported".into()),
    }
}

/// The foreground host must match the exact discovered URL to real tool output.
/// The returned receipt is a durable input to a separately scheduled download.
pub(crate) fn approve_remote_source(service:&TaskService,url:&str,evidence:&[crate::harness::ToolEvidence])->Result<String,String>{
    let parsed=reqwest::Url::parse(url).map_err(|_|"media_source_invalid")?;
    if !display::allowed_url(&parsed,&service.config.allowed_media_hosts){return Err("media_host_not_allowed".into());}
    let source=evidence.iter().find(|record|(record.name=="webfetch"||record.name.starts_with("browser_")||record.name.starts_with("deepseek-search_"))
        &&(record.input["url"].as_str()==Some(url)||record.output.contains(url))).ok_or("media_source_unverified")?;
    service.register_source(url,json!({"tool":source.name,"source_url":url,"verified_at_ms":crate::time::now_ms()}))
}

async fn render_task(context:TaskContext)->Result<TaskResult,String>{
    context.check_cancelled()?;context.progress("rendering",json!({"adapter":"pillow"}))?;
    let visual=context.task.input.get("visual").cloned().unwrap_or_else(||context.task.input.clone());
    let options=context.task.input.get("options").cloned().unwrap_or_else(||json!({}));
    let output=prepare_local(&context.service,json!({"operation":"render","visual":visual,"options":options}),Some(&context)).await?;
    let artifacts=import_outputs(&context,&output)?;
    let result=json!({"artifacts":artifacts,"metadata":output.metadata,"presentation_id":context.task.input["presentation_id"]});
    if output.metadata["visual_available"]==false{Ok(TaskResult{state:TaskState::Partial,summary:"已生成字形预览；这个字的可靠笔顺资料暂不可用。".into(),artifact_ids:artifacts.iter().map(|a|a.artifact_id.clone()).collect(),data:result})}
    else{let summary=if output.metadata["kind"]=="stroke"{format!("已根据本地笔顺资料生成“{}”的 {} 笔逐笔示意。",output.metadata["character"].as_str().unwrap_or(""),output.metadata["stroke_count"].as_u64().unwrap_or(0))}
        else{"图文素材已生成，可以预览或发送到小呆。".into()};Ok(TaskResult::success(summary,artifacts.iter().map(|a|a.artifact_id.clone()).collect(),result))}
}

async fn media_task(context:TaskContext)->Result<TaskResult,String>{
    context.check_cancelled()?;
    let input=&context.task.input;
    let source=if let Some(id)=input["artifact_id"].as_str(){context.service.artifacts.verify(id)?}
    else if let Some(id)=input["source_receipt_id"].as_str(){
        let(url,evidence)=context.service.source(id)?;context.progress("downloading",json!({"source_receipt_id":id}))?;
        let(bytes,mime)=download(&url,&context).await?;
        context.service.artifacts.put(&bytes,&mime,input["title"].as_str().unwrap_or("媒体原件"),json!({"task_id":context.task.task_id,"household_id":context.task.household_id,"source_receipt_id":id,"evidence":evidence}))?
    }else{return Err("media_source_receipt_required".into());};
    context.progress("probing",json!({"source_artifact_id":source.artifact_id}))?;
    let source_path=context.service.artifacts.path(&source.artifact_id)?;
    let output=prepare_local(&context.service,json!({"operation":"media","input_path":source_path,
        "kind":input["kind"].as_str().unwrap_or("auto"),"start_seconds":input["start_seconds"].as_f64().unwrap_or(0.0),
        "duration_seconds":input["duration_seconds"].as_f64().unwrap_or(30.0),"fps":input["fps"].as_u64().unwrap_or(5),
        "captions":input["captions"],"title":input["title"]}),Some(&context)).await?;
    context.progress("storing",json!({"files":output.files.len()}))?;
    let mut assets=import_outputs(&context,&output)?;let source_id=source.artifact_id.clone();assets.insert(0,source);
    let result=json!({"source_artifact_id":source_id,"artifacts":assets,"metadata":output.metadata,
        "playback_contract":{"video":"jpeg_frames","frame_clock":"audio_samples_16000_or_monotonic_ms","fallback":"poster_and_speech"}});
    Ok(TaskResult::success("媒体已准备好，播放由小呆当前会话的输出权限决定。",assets.iter().map(|a|a.artifact_id.clone()).collect(),result))
}

async fn download(source:&str,context:&TaskContext)->Result<(Vec<u8>,String),String>{
    let mut url=reqwest::Url::parse(source).map_err(|_|"media_source_invalid")?;
    for _ in 0..=3{
        context.check_cancelled()?;
        if !display::allowed_url(&url,&context.service.config.allowed_media_hosts){return Err("media_host_not_allowed".into());}
        let host=url.host_str().ok_or("media_host_invalid")?.to_owned();
        let addresses=tokio::net::lookup_host((host.as_str(),443)).await.map_err(|_|"media_dns_failed")?.collect::<Vec<_>>();
        if addresses.is_empty()||addresses.iter().any(|a|!display::public_address(a.ip())){return Err("media_address_invalid".into());}
        let client=reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(90))
            .resolve_to_addrs(&host,&addresses).build().map_err(|_|"media_client_failed")?;
        let mut response=client.get(url.clone()).send().await.map_err(|_|"media_download_failed")?;
        if response.status().is_redirection(){let location=response.headers().get(reqwest::header::LOCATION).and_then(|v|v.to_str().ok()).ok_or("media_redirect_invalid")?;url=url.join(location).map_err(|_|"media_redirect_invalid")?;continue;}
        if !response.status().is_success(){return Err("media_download_failed".into());}
        let cap=context.service.config.max_asset_bytes;
        if response.content_length().is_some_and(|n|n>cap){return Err("media_input_size".into());}
        let declared=response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v|v.to_str().ok()).unwrap_or("").split(';').next().unwrap_or("").trim().to_owned();
        let mut bytes=Vec::new();while let Some(chunk)=response.chunk().await.map_err(|_|"media_download_failed")?{
            context.check_cancelled()?;if bytes.len() as u64+chunk.len() as u64>cap{return Err("media_input_size".into());}bytes.extend_from_slice(&chunk);}
        let mime=sniff_mime(&bytes,&declared).ok_or("media_type_unsupported")?;
        return Ok((bytes,mime.into()));
    }Err("media_redirect_limit".into())
}
fn sniff_mime(bytes:&[u8],declared:&str)->Option<&'static str>{
    if bytes.starts_with(b"\xff\xd8\xff"){Some("image/jpeg")}
    else if bytes.starts_with(b"\x89PNG\r\n\x1a\n"){Some("image/png")}
    else if bytes.len()>12&&&bytes[0..4]==b"RIFF"&&&bytes[8..12]==b"WAVE"{Some("audio/wav")}
    else if bytes.len()>12&&&bytes[4..8]==b"ftyp"{Some("video/mp4")}
    else if bytes.starts_with(b"ID3")||(declared=="audio/mpeg"&&bytes.first()==Some(&0xff)){Some("audio/mpeg")}
    else{None}
}

/// Foreground semantic preview uses the same bounded renderer as background
/// artifacts. It does not change the already frozen speech or launch a task.
pub(crate) async fn render_preview(service:&Arc<TaskService>,visual:&Value)->Result<display::PreparedImage,String>{
    let output=prepare_local(service,json!({"operation":"render","visual":visual,"options":{"print_pdf":false}}),None).await?;
    let file=output.files.iter().find(|f|f.mime_type=="image/jpeg").ok_or("render_preview_missing")?;
    let bytes=std::fs::read(output.directory.path.join(&file.name)).map_err(|_|"render_preview_read")?;
    if bytes.len()>65_536{return Err("render_preview_size".into());}
    use sha2::{Digest,Sha256};let digest=format!("{:x}",Sha256::digest(&bytes));
    Ok(display::PreparedImage{bytes,width:480,height:272,digest,title:visual["title"].as_str().unwrap_or("图文说明").chars().take(80).collect()})
}

pub(crate) async fn stroke_info(service:&Arc<TaskService>,character:&str)->Result<Value,String>{
    if character.chars().count()!=1{return Err("stroke_character_invalid".into());}
    if service.config.stroke_data_path.is_none(){return Ok(json!({"visual_available":false,"unavailable_reason":"stroke_dataset_not_configured"}));}
    let prepared=prepare_local(service,json!({"operation":"stroke_info","character":character}),None).await?;Ok(prepared.metadata)
}

#[derive(serde::Deserialize)]
struct PreparedFile{ name:String,mime_type:String,title:String,#[serde(default)]role:String }
struct PreparedOutput{files:Vec<PreparedFile>,metadata:Value,directory:TemporaryDirectory}
struct TemporaryDirectory{path:PathBuf}
impl Drop for TemporaryDirectory{fn drop(&mut self){let _=std::fs::remove_dir_all(&self.path);}}
async fn prepare_local(service:&Arc<TaskService>,mut request:Value,context:Option<&TaskContext>)->Result<PreparedOutput,String>{
    let script=service.config.media_script.as_ref().filter(|p|p.is_file()).ok_or("media_worker_not_configured")?;
    let temporary=service.root.join("temporary");super::settings::private_dir(&temporary)?;
    let directory=TemporaryDirectory{path:temporary.join(uuid::Uuid::new_v4().simple().to_string())};super::settings::private_dir(&directory.path)?;
    request["output_dir"]=json!(directory.path);request["ffmpeg"]=json!(service.config.ffmpeg);request["ffprobe"]=json!(service.config.ffprobe);
    request["stroke_data_path"]=json!(service.config.stroke_data_path);
    let information=request["operation"]=="stroke_info";let timeout=if information{15}else{service.config.timeout_seconds.min(300)};
    request["max_output_bytes"]=json!(service.config.max_asset_bytes);request["timeout_seconds"]=json!(timeout);
    let mut command=Command::new(&service.config.python);command.arg(script).env("PYTHONDONTWRITEBYTECODE","1");
    let stdin=serde_json::to_vec(&request).map_err(|e|e.to_string())?;
    let output=run_command(command,Some(stdin),Duration::from_secs(timeout),context).await?;
    let reply:Value=serde_json::from_slice(&output.stdout).map_err(|_|"media_worker_invalid_json")?;
    if !output.success||reply["ok"]!=true{return Err(reply["error"].as_str().unwrap_or("media_worker_failed").chars().take(240).collect());}
    let files:Vec<PreparedFile>=serde_json::from_value(reply["files"].clone()).map_err(|_|"media_worker_files_invalid")?;
    if (!information&&files.is_empty())||files.len()>16{return Err("media_worker_file_limit".into());}
    let mut total=0u64;for file in &files{
        if file.name.is_empty()||file.name.len()>120||file.name.contains('/')||file.name.contains('\\')||file.name.starts_with('.')||super::artifacts::extension(&file.mime_type).is_none(){return Err("media_worker_file_invalid".into());}
        let path=directory.path.join(&file.name);let info=std::fs::symlink_metadata(&path).map_err(|_|"media_worker_file_missing")?;
        if !info.file_type().is_file(){return Err("media_worker_file_invalid".into());}total=total.checked_add(info.len()).ok_or("media_worker_output_size")?;
    }if total>service.config.max_asset_bytes{return Err("media_worker_output_size".into());}
    Ok(PreparedOutput{files,metadata:reply["metadata"].clone(),directory})
}
fn import_outputs(context:&TaskContext,output:&PreparedOutput)->Result<Vec<Artifact>,String>{
    let mut result=Vec::new();for file in &output.files{context.check_cancelled()?;
        match context.service.artifacts.import(&output.directory.path.join(&file.name),&file.mime_type,&file.title,
            json!({"task_id":context.task.task_id,"household_id":context.task.household_id,"daily_conversation_id":context.task.daily_conversation_id,
                "conversation_turn_id":context.task.conversation_turn_id,"role":file.role,"metadata":output.metadata})){
            Ok(asset)=>result.push(asset),Err(error)=>{for asset in &result{let _=context.service.artifacts.forget(&asset.artifact_id);}return Err(error);}
        }}Ok(result)
}

pub(super) struct CommandOutput{pub success:bool,pub stdout:Vec<u8>,pub stderr:Vec<u8>}
/// All child processes have fixed argv, bounded pipes and a private process
/// group. Dropping a timed-out/cancelled future also terminates FFmpeg children.
pub(super) async fn run_command(mut command:Command,input:Option<Vec<u8>>,deadline:Duration,context:Option<&TaskContext>)->Result<CommandOutput,String>{
    if let Some(context)=context{context.check_cancelled()?;}
    command.stdin(if input.is_some(){Stdio::piped()}else{Stdio::null()}).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]{use std::os::unix::process::CommandExt;command.as_std_mut().process_group(0);}
    let mut child=command.spawn().map_err(|e|format!("capability_process_unavailable: {e}"))?;
    let mut guard=ProcessGroup{pid:child.id()};
    let stdout=child.stdout.take().ok_or("capability_stdout_missing")?;let stderr=child.stderr.take().ok_or("capability_stderr_missing")?;
    let stdin=child.stdin.take();let mut cancellation=context.map(|c|c.cancelled.clone());
    let work=async{
        if let (Some(mut stream),Some(bytes))=(stdin,input){stream.write_all(&bytes).await.map_err(|_|"capability_stdin_failed")?;drop(stream);}
        let(out,err,status)=tokio::try_join!(read_bound(stdout,262_144),read_bound(stderr,16_384),async{child.wait().await.map_err(|_|"capability_wait_failed".to_owned())})?;
        Ok::<_,String>(CommandOutput{success:status.success(),stdout:out,stderr:err})
    };
    let outcome=tokio::select!{result=work=>result,_=tokio::time::sleep(deadline)=>Err("capability_timeout".into()),
        _=async{if let Some(receiver)=&mut cancellation{let _=receiver.changed().await;}else{std::future::pending::<()>().await}}=>Err("cancelled".into())};
    if outcome.is_ok(){guard.pid=None;}outcome
}
async fn read_bound(reader:impl AsyncRead+Unpin,maximum:usize)->Result<Vec<u8>,String>{
    let mut bytes=Vec::new();reader.take(maximum as u64+1).read_to_end(&mut bytes).await.map_err(|_|"capability_output_failed")?;
    if bytes.len()>maximum{Err("capability_output_limit".into())}else{Ok(bytes)}
}
struct ProcessGroup{pid:Option<u32>}
impl Drop for ProcessGroup{fn drop(&mut self){#[cfg(unix)]if let Some(pid)=self.pid{unsafe{libc::kill(-(pid as i32),libc::SIGKILL);}}}}

/// Bind prepared artifacts by creating a new immutable revision. Semantics
/// remain unchanged; replay uses this exact version rather than asking the LLM.
pub fn bind_assets(service:&TaskService,plan:&PresentationPlan,segment_id:&str,assets:Vec<String>,speech_asset_id:Option<String>)->Result<PresentationPlan,String>{
    for id in assets.iter().chain(speech_asset_id.iter()){service.artifacts.verify(id)?;}
    let mut next=plan.clone();next.revision=next.revision.checked_add(1).ok_or("presentation_revision_limit")?;
    let segment=next.segments.iter_mut().find(|s|s.segment_id==segment_id).ok_or("presentation_segment_missing")?;
    segment.asset_ids=assets;segment.speech_asset_id=speech_asset_id;service.save_plan(&next)?;Ok(next)
}
