//! Typed assistant entry point. Model text never becomes a management RPC, a
//! permission grant, an operation ID, or evidence that a mutation succeeded.
use std::sync::{Arc,OnceLock};
use chrono::{TimeZone,Timelike};
use serde::Deserialize;
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use super::{Brain,capability_registry::{CATALOG_VERSION,Snapshot},control_contracts::*};
use crate::memory::assistant_actions::ReminderAction;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Clock { timezone:Option<String> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReminder { text:String,after_seconds:Option<i64>,local_date:Option<String>,local_time:Option<String> }

pub fn proposals(structured:Option<&Value>)->Result<Vec<ActionProposal>,String>{
    let Some(value)=structured.and_then(|v|v.get("actions")) else{return Ok(vec![])};
    if value.to_string().len()>4096 {return Err("action_size_limit".into());}
    let actions:Vec<ActionProposal>=serde_json::from_value(value.clone()).map_err(|_|"action_schema_invalid")?;
    if actions.len()>1 {return Err("one_action_per_turn".into());}
    Ok(actions)
}

pub fn local_query(text:&str)->Option<ActionProposal>{
    let text=text.trim().trim_end_matches(['?','？','。','!','！']);
    let capability=match text {
        "现在几点"|"现在几点了"|"几点了"|"告诉我现在几点"=>"clock.now",
        "你能做什么"|"你现在能做什么"|"你有什么功能"=>"capability.list",
        "我有哪些提醒"|"有哪些提醒"|"看看我的提醒"|"查看提醒"=>"reminder.list",
        _=>return None,
    };
    Some(ActionProposal{capability_id:capability.into(),parameters:json!({}),target_handle:None})
}

/// Common unambiguous local commands need no model call. They still pass the
/// same typed dispatcher, source checks and durable operation transaction.
pub fn local_action(text:&str,snapshot:Option<&Snapshot>,profile:Option<&str>)->Option<ActionProposal>{
    if let Some(query)=local_query(text){return Some(query);}
    if explicit_create(text){
        if let (Some(seconds),Some((_,body)))=(relative_seconds(text),text.split_once("提醒我")){
            let body=body.trim().trim_end_matches(['。','！','!']);
            if !body.is_empty() && body.chars().count()<=80 {
                return Some(ActionProposal{capability_id:"reminder.create".into(),parameters:json!({"text":body,"after_seconds":seconds}),target_handle:None});
            }
        }
    }
    if explicit_cancel(text) && text.contains("提醒") {
        let visible=snapshot?.reminders.iter().filter(|r|r.profile_id.as_deref()==profile && r.state=="pending").collect::<Vec<_>>();
        let matching=visible.iter().copied().filter(|r|text.contains(&r.text)).collect::<Vec<_>>();
        let target=if matching.len()==1{Some(matching[0])}else if visible.len()==1{Some(visible[0])}else{None};
        // A missing target becomes a short clarification through the dispatcher.
        return Some(ActionProposal{capability_id:"reminder.cancel".into(),parameters:json!({}),target_handle:target.map(|r|format!("reminder:{}",r.id))});
    }
    None
}

pub async fn dispatch(brain:&Arc<Brain>,mut ctx:TrustedActionContext,proposal:ActionProposal,text:&str)->ActionReceipt {
    let id=proposal.capability_id.clone();let at=crate::time::now_ms() as i64;
    let requested_text=proposal.parameters.get("text").cloned();
    let result=dispatch_inner(brain,&mut ctx,proposal,text).await;
    match result {
        Ok((stage,mut result))=>{
            if id=="reminder.create" && stage==ReceiptStage::Applied {if let Some(text)=requested_text {result["reminder_text"]=text;}}
            ActionReceipt{capability_id:id,stage,operation_id:ctx.operation.map(|o|o.operation_id),result,as_of_ms:at}
        },
        Err(code)=>ActionReceipt{capability_id:id,stage:if code=="action_pending" {ReceiptStage::Unknown}else{ReceiptStage::Failed},
            operation_id:ctx.operation.map(|o|o.operation_id),result:json!({"code":code}),as_of_ms:at},
    }
}

async fn dispatch_inner(brain:&Arc<Brain>,ctx:&mut TrustedActionContext,p:ActionProposal,text:&str)->Result<(ReceiptStage,Value),String> {
    if ctx.catalog_version!=CATALOG_VERSION || ctx.service_epoch!=brain.assistant.service_epoch {return Err("action_epoch_changed".into());}
    if p.capability_id=="capability.list" {
        let _:Empty=parse(p.parameters)?;if p.target_handle.is_some(){return Err("action_target_invalid".into());}
        return Ok((ReceiptStage::ReadResult,brain.assistant.catalog(ctx.policy_epoch)));
    }
    if p.capability_id=="clock.now" {
        let args:Clock=parse(p.parameters)?;if p.target_handle.is_some(){return Err("action_target_invalid".into());}
        let snapshot=brain.assistant.snapshot(ctx.policy_epoch).ok();
        let zone=args.timezone.as_deref().or(snapshot.as_ref().map(|s|s.timezone.as_str())).unwrap_or("UTC");
        let zone=zone.parse::<chrono_tz::Tz>().map_err(|_|"clock_timezone_invalid")?;
        let now=chrono::DateTime::from_timestamp_millis(crate::time::now_ms() as i64).ok_or("clock_unavailable")?;let local=now.with_timezone(&zone);
        return Ok((ReceiptStage::ReadResult,json!({"utc":now.to_rfc3339(),"local":local.to_rfc3339(),"timezone":zone.name(),
            "timezone_source":if args.timezone.is_some(){"requested"}else{snapshot.as_ref().map(|s|s.timezone_source.as_str()).unwrap_or("fallback")},
            "as_of_ms":now.timestamp_millis(),"hour":local.hour(),"minute":local.minute(),"clock_source":"host_system","stale":false})));
    }
    let snapshot=brain.assistant.snapshot(ctx.policy_epoch)?;
    let visible=||snapshot.reminders.iter().filter(|r|r.profile_id==ctx.private_profile_id);
    if p.capability_id=="reminder.list" {
        let _:Empty=parse(p.parameters)?;if p.target_handle.is_some(){return Err("action_target_invalid".into());}
        let rows=visible().filter(|r|r.state=="pending").take(12).map(|r|json!({"handle":format!("reminder:{}",r.id),"text":r.text,"due_at_ms":r.due_at_ms,"state":r.state})).collect::<Vec<_>>();
        return Ok((ReceiptStage::ReadResult,json!({"reminders":rows,"snapshot_version":snapshot.version,"as_of_ms":snapshot.as_of_ms,
            "stale":(crate::time::now_ms() as i64)-snapshot.as_of_ms>5_000,"timezone":snapshot.timezone})));
    }
    let hash=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"capability_id":p.capability_id,"parameters":p.parameters,"target_handle":p.target_handle})).map_err(|_|"action_schema_invalid")?));
    let action=match p.capability_id.as_str(){
        "reminder.create"=>{
            if p.target_handle.is_some() || !explicit_create(text){return Err("reminder_explicit_intent_required".into());}
            let args:CreateReminder=parse(p.parameters)?;
            if args.text.trim().is_empty() || !text.contains(args.text.trim()) {return Err("reminder_text_needs_clarification".into());}
            let due=resolve_time(&args,text,&snapshot,chrono::DateTime::from_timestamp_millis(crate::time::now_ms() as i64).ok_or("clock_unavailable")?)?;
            ReminderAction::Create{text:args.text,due_at_ms:due}
        },
        "reminder.cancel"=>{
            let _:Empty=parse(p.parameters)?;
            if !explicit_cancel(text){return Err("reminder_explicit_intent_required".into());}
            let handle=p.target_handle.as_deref().ok_or("reminder_target_needs_clarification")?;
            let id=handle.strip_prefix("reminder:").ok_or("action_target_invalid")?;
            let chosen=visible().find(|r|r.id==id).ok_or("reminder_not_in_scope")?;
            let pending=visible().filter(|r|r.state=="pending").collect::<Vec<_>>();
            if pending.len()>1 && !text.contains(id) && (!text.contains(&chosen.text) || pending.iter().filter(|r|text.contains(&r.text)).count()!=1) {
                return Err("reminder_target_needs_clarification".into());
            }
            ReminderAction::Cancel{id:id.into()}
        },
        _=>return Err("capability_unavailable".into()),
    };
    let op_id=format!("op-{:x}",Sha256::digest(format!("{}:{}:0",ctx.source.conversation_turn_id,ctx.source.source_revision)));
    ctx.operation=Some(OperationRef{operation_id:op_id.clone(),operation_revision:1,task_id:None});
    static SLOTS:OnceLock<Arc<tokio::sync::Semaphore>>=OnceLock::new();
    let permit=SLOTS.get_or_init(||Arc::new(tokio::sync::Semaphore::new(4))).clone().try_acquire_owned().map_err(|_|"assistant_busy")?;
    let context=ctx.clone();let brain=Arc::clone(brain);let capability=p.capability_id;
    let work=tokio::task::spawn_blocking(move || {
        let _permit=permit;
        let operation=brain.tasks.accept_operation(&op_id,&capability,&context.source,context.private_profile_id.as_deref(),&hash)?;
        if operation.state=="applied" {
            // A retained coordinator result is not authority to resurrect an
            // effect whose source was explicitly withdrawn in the domain.
            if brain.memory.assistant_domain_receipt(&op_id,&hash)?.is_none() {return Err("operation_source_revoked".into());}
            return Ok((ReceiptStage::Applied,operation.result));
        }
        if operation.state=="failed" {return Ok((ReceiptStage::Failed,operation.result));}
        if operation.state=="unknown" {
            if let Some(receipt)=brain.memory.assistant_domain_receipt(&op_id,&hash)? {
                let saved=brain.tasks.finish_operation(&op_id,&hash,"applied",&receipt)?;
                return Ok((ReceiptStage::Applied,saved.result));
            }
            return Ok((ReceiptStage::Unknown,json!({"code":"operation_reconcile_required"})));
        }
        let result=brain.memory.assistant_reminder(&context,&hash,action);
        let (stage,value)=match result {
            Ok(receipt)=>("applied",receipt),
            Err(code)=>{
                // A storage failure may have happened after a durable effect.
                // Preserve uncertainty until the domain receipt is read.
                let state=if code.contains("store")||code.contains("database"){"unknown"}else{"failed"};
                (state,json!({"code":code}))
            }
        };
        let saved=brain.tasks.finish_operation(&op_id,&hash,stage,&value)?;
        brain.assistant.invalidate();let _=brain.assistant.refresh(&brain.memory);
        Ok::<_,String>((match saved.state.as_str(){"applied"=>ReceiptStage::Applied,"failed"=>ReceiptStage::Failed,_=>ReceiptStage::Unknown},saved.result))
    });
    // The permit stays inside the blocking worker after timeout/cancel. Durable
    // admission and evidence can finish, but cannot revive this output lease.
    tokio::time::timeout(std::time::Duration::from_secs(3),work).await.map_err(|_|"action_pending")?
        .map_err(|_|"action_pending".to_string())?.map_err(|code|if code.contains("task_store"){"action_pending".into()}else{code})
}

fn parse<T:serde::de::DeserializeOwned>(v:Value)->Result<T,String>{serde_json::from_value(v).map_err(|_|"action_parameters_invalid".into())}

fn explicit_create(text:&str)->bool {
    let s=text.trim();
    if s.chars().count()>240 || ["不要","不用","不需要","别提醒","取消","改成","修改","每天","每周","每月","每年","故事","小说","说过","说：","说:","如果","假如","假设","“","\""].iter().any(|x|s.contains(x)){return false;}
    let start=s.chars().next().unwrap_or(' ');
    let starts=["请","帮我","给我","提醒我","设个提醒","设置提醒","设一个提醒","在","今天","明天","后天","明早","今晚","过","半","一刻钟"].iter().any(|p|s.starts_with(p))
        || start.is_ascii_digit() || "一二两三四五六七八九十百".contains(start);
    starts && ["提醒我","帮我提醒","设个提醒","设置提醒","设一个提醒"].iter().any(|p|s.contains(p))
}
fn explicit_cancel(text:&str)->bool {
    let s=text.trim();
    !["不要取消","别取消","不用取消","故事","他说","她说","如果","假如","“","\""].iter().any(|v|s.contains(v))
        && ["取消","请取消","帮我取消","不要那个提醒","别再提醒我"].iter().any(|v|s.starts_with(v))
}

fn chinese_number(s:&str)->Option<i64>{
    if let Ok(v)=s.parse::<i64>(){return Some(v);}
    let digit=|c|"零一二三四五六七八九".find(c).map(|index|(index/3) as i64).or_else(||(c=='两').then_some(2));
    if s.chars().count()==1 {return if s=="十"{Some(10)}else{s.chars().next().and_then(digit)};}
    let (a,b)=s.split_once('十')?;
    let tens=if a.is_empty(){1}else{if a.chars().count()!=1{return None;}digit(a.chars().next()?)?};
    let units=if b.is_empty(){0}else{if b.chars().count()!=1{return None;}digit(b.chars().next()?)?};
    Some(tens*10+units)
}
fn relative_seconds(text:&str)->Option<i64>{
    let prefix=text.split("提醒").next()?;
    if prefix.contains("半小时后"){return Some(1800);}
    if prefix.contains("一刻钟后"){return Some(900);}
    for (unit,scale) in [("分钟后",60),("小时后",3600),("秒后",1),("天后",86400)] {
        if let Some((before,_))=prefix.split_once(unit){
            let digits=before.chars().rev().take_while(|c|c.is_ascii_digit()||"零一二两三四五六七八九十".contains(*c)).collect::<String>().chars().rev().collect::<String>();
            return chinese_number(&digits)?.checked_mul(scale);
        }
    }
    None
}
fn resolve_time(args:&CreateReminder,text:&str,snapshot:&Snapshot,now:chrono::DateTime<chrono::Utc>)->Result<i64,String>{
    if let Some(seconds)=args.after_seconds {
        if args.local_date.is_some()||args.local_time.is_some()||seconds<=0||seconds>366*86400||relative_seconds(text)!=Some(seconds){return Err("reminder_time_needs_clarification".into());}
        return Ok(now.timestamp_millis()+seconds*1000);
    }
    let zone=snapshot.timezone.parse::<chrono_tz::Tz>().map_err(|_|"reminder_timezone_unknown")?;
    if snapshot.timezone_source.starts_with("fallback") {return Err("reminder_timezone_unknown".into());}
    let date=args.local_date.as_deref().and_then(|s|chrono::NaiveDate::parse_from_str(s,"%Y-%m-%d").ok()).ok_or("reminder_time_needs_clarification")?;
    let time=args.local_time.as_deref().and_then(|s|chrono::NaiveTime::parse_from_str(s,"%H:%M").ok()).ok_or("reminder_time_needs_clarification")?;
    let today=now.with_timezone(&zone).date_naive();
    let expected_day=if text.contains("后天"){Some(today+chrono::Duration::days(2))}else if text.contains("明天")||text.contains("明早"){Some(today+chrono::Duration::days(1))}
        else if text.contains("今天")||text.contains("今晚"){Some(today)}else if text.contains(&date.format("%Y-%m-%d").to_string()){Some(date)}else{None};
    if expected_day!=Some(date) {return Err("reminder_time_needs_clarification".into());}
    let clock=time.format("%H:%M").to_string();
    let hour=time.hour();let hour12=if hour>12{hour-12}else{hour};
    let hour_words=(0..=23).find_map(|n|{
        let word=match n {0=>"零".into(),1..=9=>["","一","二","三","四","五","六","七","八","九"][n as usize].to_owned(),10=>"十".into(),11..=19=>format!("十{}",["","一","二","三","四","五","六","七","八","九"][(n-10) as usize]),20=>"二十".into(),_=>format!("二十{}",["","一","二","三"][(n-20) as usize])};
        (n==hour12 && text.contains(&format!("{word}点"))).then_some(true)
    }).unwrap_or(false);
    let half=text.contains("点半");
    let labelled=if hour>=12 {["下午","晚上","今晚","中午"].iter().any(|v|text.contains(v))}else{["上午","早上","早晨","明早","凌晨"].iter().any(|v|text.contains(v))};
    let exact=text.contains(&clock) || (time.minute()==if half{30}else{0}
        && ((labelled && (text.contains(&format!("{hour12}点"))||hour_words))
            || (hour>=13 && text.contains(&format!("{hour}点")))));
    if !exact {return Err("reminder_time_needs_clarification".into());}
    match zone.from_local_datetime(&date.and_time(time)) {
        chrono::LocalResult::Single(value) if value>now && value.timestamp_millis()<=now.timestamp_millis()+366*86_400_000=>Ok(value.timestamp_millis()),
        _=>Err("reminder_time_needs_clarification".into()),
    }
}

pub fn speech(receipt:&ActionReceipt,zone:Option<&str>)->String {
    let result=&receipt.result;
    if receipt.stage==ReceiptStage::Unknown {return "请求已收到，但还不能确认是否保存成功，可以稍后查询提醒列表。".into();}
    if receipt.stage==ReceiptStage::Failed {
        return match result["code"].as_str().unwrap_or("") {
            "reminder_time_needs_clarification"|"reminder_timezone_unknown"=>"提醒时间还不够明确，请告诉我具体日期和几点。".into(),
            "reminder_target_needs_clarification"|"reminder_not_in_scope"=>"我还不能确定你指的是哪条提醒，请说一下提醒内容。".into(),
            "reminder_explicit_intent_required"=>"这次没有新增或修改提醒。需要时，请直接说具体的提醒要求。".into(),
            "capability_unavailable"=>"这项操作目前还不能直接完成。".into(),
            _=>"这次操作还没完成，请稍后再试。".into(),
        };
    }
    match receipt.capability_id.as_str(){
        "clock.now"=>format!("{}现在是{}点{}分。",if result["timezone_source"].as_str().is_some_and(|s|s.starts_with("fallback")){"按 UTC 时间，"}else{""},result["hour"],result["minute"]),
        "capability.list"=>{
            if result["capabilities"][3]["availability"]=="ready" {"我能聊天和解释问题，也能查询时间、设置一次性提醒、查看和取消提醒。".into()}
            else {"我能聊天、解释问题和查询时间。提醒服务的状态暂时还不能确认。".into()}
        },
        "reminder.create"=>{
            let at=result["due_at_ms"].as_i64().and_then(chrono::DateTime::from_timestamp_millis);
            let zone=zone.and_then(|z|z.parse::<chrono_tz::Tz>().ok()).unwrap_or(chrono_tz::Tz::UTC);
            format!("已设置在{}提醒你{}。",at.map(|at|at.with_timezone(&zone).format("%m月%d日%H点%M分").to_string()).unwrap_or_else(||"指定时间".into()),result["reminder_text"].as_str().unwrap_or("这件事"))
        },
        "reminder.cancel"=>if result["state"]=="cancelled"{"已取消这条提醒。".into()}else{"这条提醒已经结束，没有再次取消或重放。".into()},
        "reminder.list"=>{
            let rows=result["reminders"].as_array();
            if rows.is_none_or(|v|v.is_empty()){return "最近一次记录里没有待提醒的事项。".into();}
            let rows=rows.unwrap();let items=rows.iter().take(3).filter_map(|r|r["text"].as_str()).collect::<Vec<_>>().join("、");
            format!("最近记录里有{}条待提醒事项，包括{}。",rows.len(),items)
        },
        _=>"操作结果已记录。".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reminders_require_direct_intent_and_a_host_resolved_unambiguous_time() {
        for text in ["一分钟后提醒我喝水","请在半小时后提醒我喝水","明天下午三点提醒我喝水"] {assert!(explicit_create(text),"{text}");}
        for text in ["妈妈提醒我喝水","不要提醒我喝水","如果我说提醒我喝水","故事里说一分钟后提醒我喝水","每天三点提醒我喝水"]{assert!(!explicit_create(text),"{text}");}
        assert!(!explicit_cancel("不要取消提醒"));assert!(explicit_cancel("取消刚才那个提醒"));
        let now=chrono::DateTime::parse_from_rfc3339("2026-11-27T13:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let snapshot=Snapshot{version:1,fence:1,as_of_ms:now.timestamp_millis(),policy_epoch:1,timezone:"Australia/Perth".into(),timezone_source:"configured".into(),reminders:vec![]};
        let args=CreateReminder{text:"喝水".into(),after_seconds:Some(60),local_date:None,local_time:None};
        assert_eq!(resolve_time(&args,"一分钟后提醒我喝水",&snapshot,now).unwrap(),now.timestamp_millis()+60_000);
        assert!(resolve_time(&args,"十分钟后提醒我喝水",&snapshot,now).is_err());
        let args=CreateReminder{text:"喝水".into(),after_seconds:None,local_date:Some("2026-11-28".into()),local_time:Some("15:00".into())};
        assert!(resolve_time(&args,"明天下午三点提醒我喝水",&snapshot,now).is_ok());
        assert!(resolve_time(&args,"明早提醒我喝水",&snapshot,now).is_err());
        assert!(resolve_time(&args,"明天三点提醒我喝水",&snapshot,now).is_err());
        assert!(local_query("妈妈问现在几点了").is_none());
    }

    fn fixture()->(Arc<Brain>,std::path::PathBuf){
        let root=std::env::temp_dir().join(format!("xiaodai-dispatch-{}",uuid::Uuid::new_v4()));
        (Brain::new(root.clone(),None,Arc::new(|_|{})).unwrap(),root)
    }
    fn context(brain:&Brain,id:&str,text:&str)->TrustedActionContext{
        let r=brain.memory.journal_input(&crate::memory::JournalInput{turn_id:id.into(),activation_id:"activation".into(),utterance_id:None,
            text:text.into(),occurred_at_ms:crate::time::now_ms() as i64,input_kind:"text".into(),speaker_person_id:None,private_profile_id:None,allow_cloud:false,metadata:json!({})}).unwrap();
        TrustedActionContext{request_id:id.into(),service_epoch:brain.assistant.service_epoch.clone(),source:super::super::contracts::SourceRef{
            household_id:r.household_id,daily_conversation_id:r.daily_conversation_id,episode_id:r.episode_id,conversation_turn_id:r.conversation_turn_id,
            utterance_id:r.utterance_id,source_revision:r.source_revision},private_profile_id:None,policy_epoch:r.policy_revision,snapshot_version:1,
            catalog_version:CATALOG_VERSION,foreground:None,operation:None,output_lease_id:None}
    }
    #[tokio::test]
    async fn the_real_dispatcher_creates_once_rejects_forgery_and_cancels_by_a_visible_handle(){
        let (brain,root)=fixture();let text="一分钟后提醒我喝水";
        let ctx=context(&brain,"create",text);
        let proposal=ActionProposal{capability_id:"reminder.create".into(),parameters:json!({"text":"喝水","after_seconds":60}),target_handle:None};
        let first=dispatch(&brain,ctx.clone(),proposal.clone(),text).await;
        assert_eq!(first.stage,ReceiptStage::Applied,"{:?}",first.result);
        let retry=dispatch(&brain,ctx.clone(),proposal.clone(),text).await;
        assert_eq!(first.operation_id,retry.operation_id);assert_eq!(brain.memory.reminders(10).unwrap().len(),1);
        let mut forged=proposal.clone();forged.parameters["guardian"]=json!(true);
        assert_eq!(dispatch(&brain,ctx.clone(),forged,text).await.stage,ReceiptStage::Failed);
        let mut wrong=ctx.clone();wrong.source.episode_id="another-episode".into();wrong.source.conversation_turn_id="other-source".into();
        assert_eq!(dispatch(&brain,wrong,proposal,text).await.stage,ReceiptStage::Failed);
        assert_eq!(brain.memory.reminders(10).unwrap().len(),1);
        let ctx=context(&brain,"cancel","取消那个提醒");
        let handle=format!("reminder:{}",first.result["reminder_id"].as_str().unwrap());
        let cancelled=dispatch(&brain,ctx,ActionProposal{capability_id:"reminder.cancel".into(),parameters:json!({}),target_handle:Some(handle)},"取消那个提醒").await;
        assert_eq!(cancelled.stage,ReceiptStage::Applied);assert_eq!(cancelled.result["state"],"cancelled");
        drop(brain);let _=std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn clock_and_snapshot_queries_do_not_wait_for_or_write_the_database(){
        let (brain,root)=fixture();let ctx=context(&brain,"query","现在几点");
        let db=rusqlite::Connection::open(root.join("runtime/memory/memory.sqlite3")).unwrap();db.execute_batch("BEGIN EXCLUSIVE").unwrap();
        for id in ["clock.now","capability.list","reminder.list"]{
            let result=tokio::time::timeout(std::time::Duration::from_millis(100),dispatch(&brain,ctx.clone(),ActionProposal{
                capability_id:id.into(),parameters:json!({}),target_handle:None},"现在几点")).await.unwrap();
            assert_eq!(result.stage,ReceiptStage::ReadResult,"{id}: {:?}",result.result);
            assert!(result.operation_id.is_none());
        }
        db.execute_batch("ROLLBACK").unwrap();
        let db=rusqlite::Connection::open(root.join("runtime/tasks/tasks.sqlite3")).unwrap();
        let count:i64=db.query_row("SELECT count(*) FROM assistant_operations",[],|r|r.get(0)).unwrap();assert_eq!(count,0);
        drop(db);drop(brain);let _=std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn an_actual_text_turn_answers_clock_without_invoking_a_model(){
        let (brain,root)=fixture();
        brain.accepting.store(true,std::sync::atomic::Ordering::Release);
        let input=super::super::TurnInput{turn_id:"clock-turn".into(),conversation_id:"clock-activation".into(),text:Some("现在几点".into()),wav_base64:None,profile_id:None,speak:Some(false),speaker_person_id:None,utterance_id:None,device_display:false};
        let result=brain.turn(input,true).await.unwrap();
        assert_eq!(result.provider_id,"host-assistant");assert!(result.scene.speech.contains("现在是"));
        assert!(result.task_ids.is_empty());assert!(!result.presentation.segments.is_empty());
        brain.accepting.store(false,std::sync::atomic::Ordering::Release);
        drop(brain);let _=std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn actual_text_commands_create_list_and_cancel_with_durable_receipts(){
        let (brain,root)=fixture();brain.accepting.store(true,std::sync::atomic::Ordering::Release);
        for (id,text,expected) in [("create-local","一分钟后提醒我喝水","已设置"),("list-local","我有哪些提醒","喝水"),("cancel-local","取消那个提醒","已取消")]{
            let input=super::super::TurnInput{turn_id:id.into(),conversation_id:"reminder-activation".into(),text:Some(text.into()),wav_base64:None,profile_id:None,speak:Some(false),speaker_person_id:None,utterance_id:None,device_display:false};
            let result=brain.turn(input,true).await.unwrap();
            assert_eq!(result.provider_id,"host-assistant");assert!(result.scene.speech.contains(expected),"{text}: {}",result.scene.speech);
        }
        let rows=brain.memory.reminders(10).unwrap();assert_eq!(rows.len(),1);assert_eq!(rows[0].state,"cancelled");
        brain.accepting.store(false,std::sync::atomic::Ordering::Release);drop(brain);let _=std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn unknown_operations_reconcile_domain_evidence_without_resubmission(){
        let (brain,root)=fixture();let text="一分钟后提醒我喝水";
        let proposal=ActionProposal{capability_id:"reminder.create".into(),parameters:json!({"text":"喝水","after_seconds":60}),target_handle:None};
        let hash=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"capability_id":proposal.capability_id,"parameters":proposal.parameters,"target_handle":proposal.target_handle})).unwrap()));
        let mut ctx=context(&brain,"committed",text);
        let operation_id=format!("op-{:x}",Sha256::digest(format!("{}:{}:0",ctx.source.conversation_turn_id,ctx.source.source_revision)));
        brain.tasks.accept_operation(&operation_id,"reminder.create",&ctx.source,None,&hash).unwrap();
        ctx.operation=Some(OperationRef{operation_id:operation_id.clone(),operation_revision:1,task_id:None});
        let domain=brain.memory.assistant_reminder(&ctx,&hash,ReminderAction::Create{text:"喝水".into(),due_at_ms:crate::time::now_ms() as i64+60_000}).unwrap();
        let mut absent=context(&brain,"admitted-only",text);
        let absent_id=format!("op-{:x}",Sha256::digest(format!("{}:{}:0",absent.source.conversation_turn_id,absent.source.source_revision)));
        brain.tasks.accept_operation(&absent_id,"reminder.create",&absent.source,None,&hash).unwrap();
        drop(brain);
        let brain=Brain::new(root.clone(),None,Arc::new(|_|{})).unwrap();
        ctx.service_epoch=brain.assistant.service_epoch.clone();absent.service_epoch=ctx.service_epoch.clone();
        let recovered=dispatch(&brain,ctx.clone(),proposal.clone(),text).await;
        assert_eq!(recovered.stage,ReceiptStage::Applied);assert_eq!(recovered.result["reminder_id"],domain["reminder_id"]);
        assert_eq!(dispatch(&brain,absent,proposal.clone(),text).await.stage,ReceiptStage::Unknown);
        assert_eq!(brain.memory.reminders(10).unwrap().len(),1);
        brain.memory.forget_all_household().unwrap();brain.assistant.refresh(&brain.memory).unwrap();
        ctx.policy_epoch=brain.memory.household_policy_revision().unwrap();
        assert_eq!(dispatch(&brain,ctx,proposal,text).await.stage,ReceiptStage::Failed);
        assert_eq!(brain.memory.reminders(10).unwrap()[0].state,"cancelled");
        drop(brain);let _=std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn two_reminders_require_an_unambiguous_cancel_target(){
        let (brain,root)=fixture();
        let one=brain.memory.add_reminder(&json!({"text":"喝水","due_at_ms":crate::time::now_ms()+60_000})).unwrap();
        brain.memory.add_reminder(&json!({"text":"吃饭","due_at_ms":crate::time::now_ms()+60_000})).unwrap();
        brain.assistant.refresh(&brain.memory).unwrap();
        let ctx=context(&brain,"ambiguous","取消那个提醒");
        let receipt=dispatch(&brain,ctx,ActionProposal{capability_id:"reminder.cancel".into(),parameters:json!({}),target_handle:Some(format!("reminder:{}",one.id))},"取消那个提醒").await;
        assert_eq!(receipt.result["code"],"reminder_target_needs_clarification");
        assert!(brain.memory.reminders(10).unwrap().iter().all(|r|r.state=="pending"));
        drop(brain);let _=std::fs::remove_dir_all(root);
    }
    #[test]
    fn ambiguous_and_missing_daylight_saving_times_are_not_silently_shifted(){
        let mut snapshot=Snapshot{version:1,fence:1,as_of_ms:0,policy_epoch:1,timezone:"America/New_York".into(),timezone_source:"configured".into(),reminders:vec![]};
        for (date,time,now) in [("2026-03-08","02:30","2026-03-08T05:00:00Z"),("2026-11-01","01:30","2026-11-01T04:00:00Z")] {
            let args=CreateReminder{text:"喝水".into(),after_seconds:None,local_date:Some(date.into()),local_time:Some(time.into())};
            let now=chrono::DateTime::parse_from_rfc3339(now).unwrap().with_timezone(&chrono::Utc);
            assert!(resolve_time(&args,&format!("{date} {time}提醒我喝水"),&snapshot,now).is_err());
        }
        snapshot.timezone_source="fallback".into();
        let args=CreateReminder{text:"喝水".into(),after_seconds:None,local_date:Some("2026-11-28".into()),local_time:Some("15:00".into())};
        assert!(resolve_time(&args,"明天15:00提醒我喝水",&snapshot,chrono::DateTime::from_timestamp_millis(0).unwrap()).is_err());
    }
}
