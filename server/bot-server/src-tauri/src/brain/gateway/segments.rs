//! One in-flight segment. TCP completion is not a rendered/played receipt.
use super::*;
use std::time::Duration;
use crate::brain::{orchestration::PreparedSegment,presentation::{PresentationPlan,OutputLease,PlaybackPurpose}};

pub(super) struct Delivery {
    pub plan:PresentationPlan,pub token:RuntimeToken,pub turn_id:String,pub activation_id:String,
    pub lease_id:String,pub playback_id:String,pub index:u32,pub current_seq:u64,pub total_sent:u32,
    pub pending:VecDeque<PreparedSegment>,pub current:Option<String>,pub tasks:VecDeque<String>,
    pub audio_requested:bool,pub started:Instant,pub last_progress:Instant,pub preparing_task:bool,
    pub request_id:String,pub progress_seq:u64,pub progress_only:bool,
    /// The producer can still add speech. A segment receipt alone cannot finish this turn.
    pub live_content:bool,
    pub(super) current_spoken:Option<String>,
    /// When the current segment was handed to the device, and how long it may
    /// play before a missing receipt releases the foreground.
    pub report_since:Option<Instant>,
    pub report_budget:Duration,
    receipt_sequence:u64,current_rendered:Option<String>,current_visual_unavailable:bool,
}
impl Delivery {
    pub fn new(brain:&Brain,device:&str,activation:&str,token:RuntimeToken,turn_id:String,plan:PresentationPlan,
        segments:Vec<PreparedSegment>,tasks:Vec<String>,audio:bool,index:u32)->Result<Self,String>{
        if segments.is_empty()||segments.len()>64||index==0||index>64{return Err("presentation_segment_limit".into());}
        let lease_id=format!("lease_{}",uuid::Uuid::new_v4().simple());
        brain.tasks.grant_output(OutputLease{output_lease_id:lease_id.clone(),device_id:device.into(),activation_id:activation.into(),
            runtime:serde_json::to_value(token).map_err(|e|e.to_string())?,purpose:if tasks.is_empty(){PlaybackPurpose::Content}else{PlaybackPurpose::TaskAck},
            expires_at_ms:crate::time::now_ms() as i64+120_000,revoked:false})?;
        let playback_id=brain.tasks.begin_playback(&plan,&lease_id)?;
        Ok(Self{plan,token,turn_id,activation_id:activation.into(),lease_id,playback_id,index,current_seq:0,total_sent:0,pending:segments.into(),current:None,
            tasks:tasks.into(),audio_requested:audio,started:Instant::now(),last_progress:Instant::now(),preparing_task:false,request_id:String::new(),progress_seq:1000,progress_only:false,live_content:false,report_since:None,report_budget:segment_report_budget(0,0),receipt_sequence:0,current_spoken:None,current_rendered:None,current_visual_unavailable:false})
    }
    pub fn next(&mut self,brain:&Brain)->Result<Option<(Value,Option<Playback>,DisplayResult)>,String>{
        if !brain.memory.pending_household_cleanup()?.is_empty(){return Err("source_cleanup_pending".into());}
        if !brain.tasks.output_active(&self.lease_id)? || !brain.tasks.source_current(&self.plan.context.conversation_turn_id)? {return Err("presentation_lease_revoked".into());}
        if self.current.is_some(){return Ok(None);}
        let Some(segment)=self.pending.pop_front() else{return Ok(None)};
        if self.started.elapsed()>Duration::from_secs(120){return Err("presentation_expired".into());}
        if self.total_sent>=64{return Err("presentation_turn_segment_budget".into());}
        self.total_sent+=1;self.current_seq+=1;self.current=Some(segment.segment_id.clone());
        let final_segment=!self.live_content && !self.progress_only&&self.pending.is_empty()&&self.tasks.is_empty();
        let purpose=if self.progress_only{"progress"}else if final_segment{"final"}else if self.index==1&&!self.tasks.is_empty(){"task_ack"}else if self.pending.is_empty()&&!self.tasks.is_empty(){"progress"}else{"content"};
        let pcm=if self.audio_requested {segment.audio.as_ref().and_then(|v|v["wav_base64"].as_str()).map(media::decode_output).transpose()?}else{None};
        let stream_id=u32::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().map_err(|_|"stream_id")?).max(1);
        let descriptor=pcm.as_ref().map(|pcm|json!({"stream_id":stream_id,"format":"pcm_s16le","sample_rate":16000,"channels":1,
            "bytes":pcm.len(),"frame_samples":320,"playback_ack":"hardware_v1"}));
        let image=segment.display_image.map(|image|(format!("img-{}",uuid::Uuid::new_v4().simple()),image));
        let cards=image.as_ref().map(|(id,image)|vec![json!({"kind":"image","title":image.title,"body":"",
            "asset":{"id":id,"mime":"image/jpeg","width":image.width,"height":image.height,"bytes":image.bytes.len(),"sha256":image.digest}})]).unwrap_or_default();
        let mut speech=segment.scene.speech;
        let frozen=self.plan.segments.iter().find(|part|part.segment_id==segment.segment_id).ok_or("presentation_segment_not_in_plan")?;
        // Original media PCM is not a TTS rendering of its title/caption.
        let spoken=if pcm.is_some() && frozen.speech_asset_id.is_some() && !frozen.speech.is_empty() && frozen.speech==speech{Some(speech.clone())}else{None};
        self.current_visual_unavailable=frozen.wait_for_first_frame && frozen.visual["kind"]!="none" && image.is_none();
        while speech.len()>DEVICE_CAPTION_BYTES {speech.pop();}
        // The retained caption remains readable after a voice-only answer. The
        // hardware's actual playback and display receipts still gate follow-up.
        let voice_only=spoken.is_some() && image.is_none() && segment.scene.cards.is_empty()
            && segment.media_kind.is_none() && frozen.visual["kind"]=="none" && !frozen.wait_for_first_frame;
        let hold_ms=if voice_only {0} else {segment.dwell_ms.min(30_000)};
        self.report_budget=segment_report_budget(pcm.as_ref().map(|bytes| bytes.len()).unwrap_or(0), hold_ms);
        self.report_since=Some(Instant::now());
        self.current_spoken=spoken.clone();
        self.current_rendered=Some(spoken.unwrap_or_else(||speech.clone()));
        let mut payload=json!({"presentation_id":self.plan.presentation_id,"presentation_index":self.index,"output_lease_id":self.lease_id,
            "segment_id":segment.segment_id,"seq":self.current_seq,"purpose":purpose,"final":final_segment,
            "hold_ms":hold_ms,"scene":{"speech":speech,"expression":segment.scene.expression,"cards":cards},"audio":descriptor});
        if let Some(kind)=&segment.media_kind{payload["media_kind"]=json!(kind);}
        self.receipt_sequence+=1;
        let current=brain.tasks.record_receipt(crate::brain::presentation::PlaybackReceipt{
            receipt_id:format!("receipt_{}",uuid::Uuid::new_v4().simple()),playback_id:self.playback_id.clone(),presentation_id:self.plan.presentation_id.clone(),
            presentation_revision:self.plan.revision,segment_id:segment.segment_id,output_lease_id:self.lease_id.clone(),sequence:self.receipt_sequence,
            state:"prepared".into(),device_monotonic_ms:None,sample_position:None,detail:json!({"index":self.index})})?;
        if !current{return Err("presentation_lease_revoked".into());}
        let playback=pcm.map(|pcm|Playback{turn_id:self.turn_id.clone(),stream_id,token:self.token,pcm,offset:0,sequence:0,hardware_receipts:true,
            ready:false,sent_complete:false,hardware_started:false,created:Instant::now(),ready_at:None,ready_timeout:Duration::from_secs(if image.is_some(){30}else{5})});
        Ok(Some((json!({"v":3,"type":"event","event":"presentation.segment","runtime":self.token,"payload":payload}),playback,
            DisplayResult{token:self.token,turn_id:self.turn_id.clone(),source_turn_id:Some(self.plan.context.conversation_turn_id.clone()),output_lease_id:Some(self.lease_id.clone()),image,delivered:Instant::now()})))
    }
    /// Receipt sequence is independent of segment sequence: a segment can
    /// produce several genuine display/audio facts before aggregate completion.
    pub fn record_display(&mut self,brain:&Brain,part:&str,status:&str,detail:Value)->Result<(),String>{
        if status=="unavailable" && part=="image" {self.current_visual_unavailable=true;}
        self.record_hardware(brain,if status=="rendered"{"displayed"}else{"failed"},detail)
    }
    pub fn record_hardware(&mut self,brain:&Brain,state:&str,detail:Value)->Result<(),String>{
        let segment=self.current.clone().ok_or("presentation_not_waiting")?;
        self.receipt_sequence+=1;
        let current=brain.tasks.record_receipt(crate::brain::presentation::PlaybackReceipt{
            receipt_id:format!("receipt_{}",uuid::Uuid::new_v4().simple()),playback_id:self.playback_id.clone(),presentation_id:self.plan.presentation_id.clone(),
            presentation_revision:self.plan.revision,segment_id:segment,output_lease_id:self.lease_id.clone(),sequence:self.receipt_sequence,
            state:state.into(),device_monotonic_ms:detail["device_monotonic_ms"].as_u64(),sample_position:detail["sample_position"].as_u64(),detail})?;
        if !current{return Err("presentation_lease_revoked".into());}Ok(())
    }
    pub fn report(&mut self,brain:&Brain,payload:&Value)->Result<bool,String>{
        let token=parse_runtime(payload)?;
        if token!=self.token||payload["presentation_id"]!=self.plan.presentation_id||payload["output_lease_id"]!=self.lease_id
            ||payload["presentation_index"].as_u64()!=Some(self.index as u64)||payload["seq"].as_u64()!=Some(self.current_seq)||payload["segment_id"].as_str()!=self.current.as_deref(){return Err("presentation_receipt_stale".into());}
        let status=payload["status"].as_str().filter(|s|matches!(*s,"completed"|"failed")).ok_or("presentation_receipt_status")?;
        let segment=self.current.as_ref().ok_or("presentation_not_waiting")?;
        self.receipt_sequence+=1;
        let current=brain.tasks.record_receipt(crate::brain::presentation::PlaybackReceipt{
            receipt_id:format!("{}_{}",self.playback_id,self.current_seq),playback_id:self.playback_id.clone(),presentation_id:self.plan.presentation_id.clone(),
            presentation_revision:self.plan.revision,segment_id:segment.clone(),output_lease_id:self.lease_id.clone(),sequence:self.receipt_sequence,
            state:status.into(),device_monotonic_ms:payload["device_monotonic_ms"].as_u64(),sample_position:payload["sample_position"].as_u64(),detail:json!({"code":payload["code"],"visual_unavailable":self.current_visual_unavailable})})?;
        let _=brain.memory.journal_receipt(&self.turn_id,"presentation",json!({"presentation_id":self.plan.presentation_id,
            "segment_id":segment,"status":status,"code":payload["code"],"visual_unavailable":self.current_visual_unavailable,"rendered_text":if status=="completed"{self.current_rendered.as_deref()}else{None}}),&format!("{}_{}",self.playback_id,self.current_seq));
        self.current=None;self.current_rendered=None;self.current_spoken=None;self.report_since=None;
        if !current{return Err("presentation_lease_revoked".into());}
        if status=="failed" {self.pending.clear();return Err("presentation_segment_failed".into());}
        Ok(self.pending.is_empty()&&self.tasks.is_empty())
    }
    /// RA-00: a delivery that is only waiting on background tasks does not make
    /// the next foreground input session_busy. While the switch is off, any
    /// delivery blocks, which is the baseline.
    pub(super) fn blocks_new_input(&self, quick_decouple: bool) -> bool {
        if !quick_decouple { return true; }
        self.current.is_some() || !self.pending.is_empty() || self.live_content || self.progress_only
    }
    pub(super) fn report_expired(&self) -> bool {
        self.current.is_some() && self.report_since.is_some_and(|sent| sent.elapsed() > self.report_budget)
    }
}

/// Audio is sent in real time and a card may dwell afterwards. The receipt
/// deadline is that expected play time plus a fixed margin, so a sentence
/// longer than a few seconds is not torn down while it is still playing.
pub(super) fn segment_report_budget(pcm_bytes: usize, hold_ms: u64) -> Duration {
    let audio_ms = (pcm_bytes as u64).saturating_mul(1000) / 32_000;
    Duration::from_millis(audio_ms.saturating_add(hold_ms).saturating_add(REPORT_MARGIN_MS))
}

const REPORT_MARGIN_MS: u64 = 10_000;

/// The pending result or clarification is the first part of the device turn's
/// presentation, under that turn's plan, so the firmware can accept it while
/// the turn is COMMITTED.
pub(super) fn prepend_lead(brain:&Brain, lead_plan:PresentationPlan, lead_segments:Vec<PreparedSegment>, mut answer:PresentationPlan, answer_segments:Vec<PreparedSegment>)->Result<(PresentationPlan,Vec<PreparedSegment>),String>{
    let mut semantic=lead_plan.segments;
    semantic.extend(answer.segments);
    if semantic.is_empty()||semantic.len()>64 { return Err("presentation_segment_limit".into()); }
    answer.segments=semantic;
    answer.presentation_id=format!("presentation_{}",uuid::Uuid::new_v4().simple());
    answer.revision=1;
    use sha2::{Digest,Sha256};
    answer.digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"context":answer.context,"segments":answer.segments})).map_err(|e|e.to_string())?));
    brain.tasks.save_plan(&answer)?;
    let mut prepared=lead_segments;
    prepared.extend(answer_segments);
    Ok((answer,prepared))
}

pub(super) fn restore(brain:&Brain,plan:&PresentationPlan)->Result<Vec<PreparedSegment>,String>{
    let mut output=Vec::new();
    for segment in &plan.segments {
        let audio=if let Some(id)=&segment.speech_asset_id {
            let asset=brain.tasks.artifacts.verify(id)?;
            if asset.mime_type!="audio/wav"||asset.bytes>8_000_000{return Err("presentation_audio_asset_invalid".into());}
            let bytes=std::fs::read(brain.tasks.artifacts.path(id)?).map_err(|_|"presentation_audio_missing")?;
            Some(json!({"wav_base64":STANDARD.encode(bytes)}))
        }else{None};
        let mut image=None;
        for id in &segment.asset_ids {
            let asset=brain.tasks.artifacts.verify(id)?;
            if asset.mime_type=="image/jpeg"&&asset.bytes<=65536 {
                let bytes=brain.tasks.artifacts.read(id,0,65536)?;
                let width=asset.source.pointer("/metadata/width").and_then(Value::as_u64).filter(|v|*v>0&&*v<=480).unwrap_or(480) as u32;
                let height=asset.source.pointer("/metadata/height").and_then(Value::as_u64).filter(|v|*v>0&&*v<=272).unwrap_or(272) as u32;
                image=Some(crate::brain::display::PreparedImage{bytes,width,height,digest:asset.digest,title:asset.title});break;
            }
        }
        let scene=scene_for(if segment.speech.is_empty(){&segment.caption}else{&segment.speech},&segment.expression);
        output.push(PreparedSegment{segment_id:segment.segment_id.clone(),scene,audio,display_image:image,dwell_ms:segment.dwell_ms,media_kind:segment.visual["media_kind"].as_str().map(str::to_owned)});
    }
    Ok(output)
}
/// A spoken, in-character line for a device turn that failed before anything was
/// said. `None` keeps the device's caption-only failure path.
pub(super) async fn prepare_failure(brain:&Brain,turn_id:&str,error:&str)->Result<Option<(PresentationPlan,Vec<PreparedSegment>)>,String>{
    let Some((line,expression))=crate::brain::voice_lines::failure_line(error,turn_id) else{return Ok(None)};
    let household=brain.memory.household_settings()?["household_id"].as_str().unwrap_or_default().to_owned();
    let daily=brain.memory.daily_conversation_id(crate::time::now_ms() as i64)?;
    let context=crate::brain::presentation::PlanContext{household_id:household,daily_conversation_id:daily,conversation_turn_id:turn_id.into(),task_id:None};
    let cache=brain.failure_audio.lock().await.clone();
    let prepared=tokio::time::timeout(Duration::from_secs(4),
        brain.prepare_presentation(context,scene_for(line,expression),None,&[],"",true,&[],None,Some(&cache))).await
        .map_err(|_|"failure_speech_timeout".to_string())??;
    let (plan,segments,_)=prepared;
    if segments.len()!=1 || segments[0].audio.is_none() {return Ok(None);}
    if let Some(audio)=segments[0].audio.clone() {brain.failure_audio.lock().await.insert(line.into(),audio);}
    Ok(Some((plan,segments)))
}

/// An unprompted line (UX review §12.2), synthesised like the failure line. It is
/// not a household turn: its receipts are not journalled there (the turn id is
/// unknown to memory) and the proactive journal records it with its trigger.
pub(super) async fn prepare_proactive(brain:&Brain,turn_id:&str,line:&str,local_date:&str)->Result<(PresentationPlan,Vec<PreparedSegment>),String>{
    let household=brain.memory.household_settings()?["household_id"].as_str().unwrap_or_default().to_owned();
    // A synthetic day id: never create an empty household day just to greet.
    let context=crate::brain::presentation::PlanContext{household_id:household,daily_conversation_id:format!("proactive-{local_date}"),
        conversation_turn_id:turn_id.into(),task_id:None};
    let (plan,segments,_)=tokio::time::timeout(Duration::from_secs(8),
        brain.prepare_presentation(context,scene_for(line,"happy"),None,&[],"",true,&[],None,None)).await
        .map_err(|_|"proactive_speech_timeout".to_string())??;
    if segments.len()!=1 || segments[0].audio.is_none() {return Err("proactive_speech_unavailable".into());}
    Ok((plan,segments))
}

pub(super) fn scene_for(speech:&str,expression:&str)->crate::brain::scene::Scene{
    crate::brain::scene::normalize("",Some(&json!({"speech":speech,"expression":expression,"cards":[]})),4000,&[])
}

pub(super) struct TaskReady {
    pub token:RuntimeToken,pub activation_id:String,pub task_id:String,pub progress_only:bool,
    pub result:Result<(PresentationPlan,Vec<PreparedSegment>),String>,
}
pub(super) async fn prepare_task(brain:Arc<Brain>,task:crate::brain::tasks::TaskRecord)->Result<(PresentationPlan,Vec<PreparedSegment>),String>{
    if matches!(task.state,crate::brain::tasks::TaskState::Succeeded|crate::brain::tasks::TaskState::Partial){
        if let Some((mut plan,segments))=prepare_media_result(&brain,&task)? {
            if task.capability=="presentation.render" {
                // A prepared silent diagram/stroke animation still needs an
                // audible, factual introduction. It is a separate frozen
                // segment, never a rewrite of an existing media soundtrack.
                let speech=task.result["summary"].as_str().unwrap_or("这份图文资料已经准备好了。");
                let (intro,mut spoken,_)=brain.prepare_presentation(plan.context.clone(),scene_for(speech,"happy"),None,&[],"",true,&[],None,None).await?;
                let mut semantic=intro.segments;semantic.extend(plan.segments);
                plan.segments=semantic;plan.presentation_id=format!("presentation_{}",uuid::Uuid::new_v4().simple());
                use sha2::{Digest,Sha256};
                plan.digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"context":plan.context,"segments":plan.segments})).map_err(|e|e.to_string())?));
                brain.tasks.save_plan(&plan)?;spoken.extend(segments);return Ok((plan,spoken));
            }
            return Ok((plan,segments));
        }
        if let Some(id)=task.result.pointer("/data/presentation_id").and_then(Value::as_str){
            let plan=brain.tasks.plan(id,None)?;let segments=restore(&brain,&plan)?;return Ok((plan,segments));
        }
    }
    let summary=match task.state {
        crate::brain::tasks::TaskState::Failed=>"这件事暂时没有完成，可以在 Mac 上查看原因。".into(),
        crate::brain::tasks::TaskState::Cancelled=>"这项任务已经取消。".into(),
        crate::brain::tasks::TaskState::Unknown=>"暂时还不能确认结果，我保留了任务记录，请在 Mac 上检查。".into(),
        crate::brain::tasks::TaskState::AwaitingInput=>task.result["summary"].as_str().unwrap_or("还需要补充一些信息，请在 Mac 上查看任务。可以接着告诉我缺少的参数。").to_owned(),
        crate::brain::tasks::TaskState::Succeeded|crate::brain::tasks::TaskState::Partial=>task.result["summary"].as_str().unwrap_or("任务有了新的结果，可以在 Mac 上查看。").to_owned(),
        _=>"任务仍在处理中。".into(),
    };
    let context=crate::brain::presentation::PlanContext{household_id:task.household_id,daily_conversation_id:task.daily_conversation_id,
        conversation_turn_id:task.conversation_turn_id,task_id:Some(task.task_id)};
    let expression=if matches!(task.state,crate::brain::tasks::TaskState::Succeeded|crate::brain::tasks::TaskState::Partial){"happy"}else{"thinking"};
    let (plan,segments,_)=brain.prepare_presentation(context,scene_for(&summary,expression),None,&[],"",true,&[],None,None).await?;
    Ok((plan,segments))
}

/// ESP receives a bounded, back-pressured 12-second excerpt. The complete
/// original asset remains available in the Mac console; no real-time FPS claim.
fn prepare_media_result(brain:&Brain,task:&crate::brain::tasks::TaskRecord)->Result<Option<(PresentationPlan,Vec<PreparedSegment>)>,String>{
    if !matches!(task.capability.as_str(),"media.prepare"|"presentation.render"){return Ok(None);}
    let Some(items)=task.result.pointer("/data/artifacts").and_then(Value::as_array) else{return Ok(None)};
    let assets=items.iter().map(|v|serde_json::from_value::<crate::brain::artifacts::Artifact>(v.clone()).map_err(|_|"task_artifacts_invalid".to_owned())).collect::<Result<Vec<_>,_>>()?;
    let video=assets.iter().find(|a|a.source["role"]=="video");
    let index=assets.iter().find(|a|a.source["role"]=="frame_index");
    let audio=assets.iter().find(|a|a.source["role"]=="audio");
    let poster=assets.iter().find(|a|a.source["role"]=="poster");
    let pcm=if let Some(asset)=audio {
        brain.tasks.artifacts.verify(&asset.artifact_id)?;
        let bytes=std::fs::read(brain.tasks.artifacts.path(&asset.artifact_id)?).map_err(|_|"media_audio_missing")?;
        Some(media::decode_output(&STANDARD.encode(bytes))?)
    }else{None};
    let mut prepared=Vec::new();let mut segments=Vec::new();
    let context=crate::brain::presentation::PlanContext{household_id:task.household_id.clone(),daily_conversation_id:task.daily_conversation_id.clone(),
        conversation_turn_id:task.conversation_turn_id.clone(),task_id:Some(task.task_id.clone())};
    let presentation_id=format!("presentation_{}",uuid::Uuid::new_v4().simple());
    if let (Some(video),Some(index))=(video,index){
        brain.tasks.artifacts.verify(&video.artifact_id)?;brain.tasks.artifacts.verify(&index.artifact_id)?;
        let entries:Value=serde_json::from_slice(&brain.tasks.artifacts.read(&index.artifact_id,0,262144)?).map_err(|_|"media_frame_index_invalid")?;
        let frames=entries["frames"].as_array().ok_or("media_frame_index_invalid")?;
        let mut chosen=Vec::new();let mut last=None;
        for frame in frames {
            let at=frame["at_ms"].as_u64().ok_or("media_frame_time")?;
            if at>=12000{break;}
            if last.is_some_and(|prior:u64|at.saturating_sub(prior)<200){continue;}
            chosen.push(frame);last=Some(at);if chosen.len()>=60{break;}
        }
        for (i,frame) in chosen.iter().enumerate(){
            let offset=frame["offset"].as_u64().ok_or("media_frame_offset")?;
            let size=frame["bytes"].as_u64().filter(|n|*n>0&&*n<=65536).ok_or("media_frame_size")? as usize;
            let bytes=brain.tasks.artifacts.read(&video.artifact_id,offset,size)?;
            if bytes.len()!=size||!bytes.starts_with(&[0xff,0xd8])||!bytes.ends_with(&[0xff,0xd9]){return Err("media_frame_invalid".into());}
            let at=frame["at_ms"].as_u64().ok_or("media_frame_time")?;
            let end=chosen.get(i+1).and_then(|v|v["at_ms"].as_u64()).unwrap_or(at+200).min(12000);
            let segment_id=format!("segment_{}",uuid::Uuid::new_v4().simple());
            let image=brain.tasks.artifacts.put(&bytes,"image/jpeg",&video.title,json!({"conversation_turn_id":task.conversation_turn_id,
                "task_id":task.task_id,"source_artifact_id":video.artifact_id,"source_frame":frame["frame"],"metadata":{"width":480,"height":272}}))?;
            let clip=pcm.as_ref().and_then(|pcm|{
                let start=(at as usize*32).min(pcm.len());let end=(end as usize*32).min(pcm.len());
                (end>start).then(||media::pcm_wav(&pcm[start..end]))
            });
            let speech_asset_id=clip.as_ref().map(|bytes|brain.tasks.artifacts.put(bytes,"audio/wav",&video.title,json!({"conversation_turn_id":task.conversation_turn_id,"task_id":task.task_id,"source_at_ms":at})).map(|a|a.artifact_id)).transpose()?;
            let caption=if i==0{format!("{}（分段预览；完整媒体见 Mac）",video.title)}else{video.title.clone()};
            segments.push(crate::brain::presentation::PresentationSegment{segment_id:segment_id.clone(),speech:String::new(),caption:caption.clone(),expression:"happy".into(),
                visual:json!({"kind":"artifact","artifact_id":image.artifact_id,"media_kind":"mjpeg_frame","source_at_ms":at}),visual_required:true,wait_for_first_frame:true,dwell_ms:200,
                fallback:"poster_and_mac_original".into(),asset_ids:vec![image.artifact_id],speech_asset_id});
            prepared.push(PreparedSegment{segment_id,scene:scene_for(&caption,"happy"),audio:clip.map(|v|json!({"wav_base64":STANDARD.encode(v)})),
                display_image:Some(crate::brain::display::PreparedImage{bytes,width:480,height:272,digest:image.digest,title:image.title}),dwell_ms:200,media_kind:Some("mjpeg_frame".into())});
        }
    }else if poster.is_some()||pcm.is_some(){
        let image=poster.map(|asset|{
            brain.tasks.artifacts.verify(&asset.artifact_id)?;
            let bytes=brain.tasks.artifacts.read(&asset.artifact_id,0,65536)?;
            if asset.bytes>65536{return Err("media_poster_limit".to_owned());}
            Ok(crate::brain::display::PreparedImage{bytes,width:480,height:272,digest:asset.digest.clone(),title:asset.title.clone()})
        }).transpose()?;
        let segment_id=format!("segment_{}",uuid::Uuid::new_v4().simple());
        let caption=poster.or(audio).map(|a|a.title.as_str()).unwrap_or("准备好的结果");
        let audio_value=pcm.map(|pcm|json!({"wav_base64":STANDARD.encode(media::pcm_wav(&pcm))}));
        segments.push(crate::brain::presentation::PresentationSegment{segment_id:segment_id.clone(),speech:String::new(),caption:caption.into(),expression:"happy".into(),
            visual:json!({"kind":"artifact"}),visual_required:poster.is_some(),wait_for_first_frame:poster.is_some(),dwell_ms:2500,fallback:"mac_original".into(),
            asset_ids:poster.map(|a|vec![a.artifact_id.clone()]).unwrap_or_default(),speech_asset_id:audio.map(|a|a.artifact_id.clone())});
        prepared.push(PreparedSegment{segment_id,scene:scene_for(caption,"happy"),audio:audio_value,display_image:image,dwell_ms:2500,media_kind:None});
    }
    if prepared.is_empty(){return Ok(None);}
    use sha2::{Digest,Sha256};
    let digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"context":context,"segments":segments})).map_err(|e|e.to_string())?));
    let plan=PresentationPlan{schema_version:1,presentation_id,revision:1,digest,context,segments,created_at_ms:crate::time::now_ms() as i64};
    brain.tasks.save_plan(&plan)?;Ok(Some((plan,prepared)))
}

pub(super) async fn prepare_progress(brain:Arc<Brain>,task:crate::brain::tasks::TaskRecord)->Result<(PresentationPlan,Vec<PreparedSegment>),String>{
    let message=match task.stage.as_str(){
        "downloading"=>"我正在下载找到的资料。", "probing"=>"我正在检查这段媒体能不能播放。",
        "rendering"|"preparing_media"=>"我正在把结果整理成画面。", "submitting"=>"我正在把这份内容交给打印机。",
        "printing"|"queued"|"monitoring"=>"我在等打印任务的状态，暂时还不能确认已经打印。",
        _=>"我还在处理这件事，结果回来就告诉你。",
    };
    let cached={brain.progress_audio.lock().await.get(message).cloned()};
    let audio=if let Some(audio)=cached{audio}else{
        let audio=tokio::time::timeout(Duration::from_secs(4),brain.models.call("tts_first",json!({"text":message,"voice":brain.models.voice()}))).await.map_err(|_|"progress_voice_deferred")??;
        brain.progress_audio.lock().await.insert(message.into(),audio.clone());audio
    };
    let context=crate::brain::presentation::PlanContext{household_id:task.household_id.clone(),daily_conversation_id:task.daily_conversation_id.clone(),conversation_turn_id:task.conversation_turn_id.clone(),task_id:Some(task.task_id.clone())};
    let mut plan=crate::brain::presentation::PresentationPlanner::default().freeze(context,&scene_for(message,"thinking"),None,false)?;
    let bytes=STANDARD.decode(audio["wav_base64"].as_str().ok_or("progress_wav_missing")?).map_err(|_|"progress_wav_invalid")?;
    let asset=brain.tasks.artifacts.put(&bytes,"audio/wav","进度语音",json!({"conversation_turn_id":task.conversation_turn_id,"task_id":task.task_id,"presentation_id":plan.presentation_id}))?;
    let segment=plan.segments.first_mut().ok_or("presentation_empty")?;segment.speech_asset_id=Some(asset.artifact_id);segment.dwell_ms=500;
    let prepared=vec![PreparedSegment{segment_id:segment.segment_id.clone(),scene:scene_for(message,"thinking"),audio:Some(audio),display_image:None,dwell_ms:500,media_kind:None}];
    brain.tasks.save_plan(&plan)?;Ok((plan,prepared))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_or_duplicate_receipts_cannot_advance_the_live_segment() {
        use crate::brain::presentation::{PlanContext, PresentationPlanner};

        let dir = std::env::temp_dir().join(format!("xiaodai-receipts-{}", uuid::Uuid::new_v4()));
        let brain = Brain::new(dir.clone(), None, Arc::new(|_| {})).unwrap();
        let scene = scene_for("第一句。", "thinking");
        let context = PlanContext {
            household_id: "house".into(), daily_conversation_id: "day".into(),
            conversation_turn_id: "turn".into(), task_id: None,
        };
        let content = json!({"presentation":{"segments":[{"speech":"第一句。"},{"speech":"第二句。"}]}});
        let plan = PresentationPlanner::default().freeze(context, &scene, Some(&content), false).unwrap();
        let start = |token| Delivery::new(&brain, "device", "activation", token, "turn".into(),
            plan.clone(), restore(&brain, &plan).unwrap(), vec![], false, 1).unwrap();
        let receipt = |delivery: &mut Delivery| {
            let (message, _, _) = delivery.next(&brain).unwrap().unwrap();
            let mut report = message["payload"].clone();
            report["runtime"] = message["runtime"].clone();
            report["status"] = json!("completed");
            report
        };

        let token = RuntimeToken { epoch: 7, session_id: 1, turn_id: 1, focus_generation: 0 };
        let mut retired = start(token);
        let late = receipt(&mut retired);
        brain.tasks.revoke_outputs(Some("activation")).unwrap();
        assert_eq!(retired.report(&brain, &late).unwrap_err(), "presentation_lease_revoked");

        // Replay the same plan. Plan, segment, index and sequence coincide on
        // the first segment; the runtime token and output lease must each match.
        let mut live = start(RuntimeToken { epoch: 8, turn_id: 2, ..token });
        let first = receipt(&mut live);
        for key in ["runtime", "output_lease_id"] {
            let mut stale = first.clone();
            stale[key] = late[key].clone();
            assert_eq!(live.report(&brain, &stale).unwrap_err(), "presentation_receipt_stale");
            assert!(live.next(&brain).unwrap().is_none());
        }
        assert!(!live.report(&brain, &first).unwrap());
        let last = receipt(&mut live);
        assert_eq!(last["seq"], 2);
        for _ in 0..2 {
            assert_eq!(live.report(&brain, &first).unwrap_err(), "presentation_receipt_stale");
            assert!(live.next(&brain).unwrap().is_none());
        }
        assert!(live.report(&brain, &last).unwrap());
        assert_eq!(live.report(&brain, &last).unwrap_err(), "presentation_receipt_stale");
        drop(brain);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_delegated_explanation_is_a_final_segment_and_does_not_hold_the_next_turn() {
        use crate::brain::presentation::{PlanContext, PresentationPlanner};
        let dir = std::env::temp_dir().join(format!("xiaodai-delegate-{}", uuid::Uuid::new_v4()));
        let brain = Brain::new(dir.clone(), None, Arc::new(|_| {})).unwrap();
        let scene = scene_for("好的，我接到这件事了，正在处理，有结果就告诉你。", "thinking");
        let context = PlanContext { household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn".into(), task_id: Some("task-1".into()) };
        let plan = PresentationPlanner::default().freeze(context, &scene, None, false).unwrap();
        let token = RuntimeToken { epoch: 1, session_id: 1, turn_id: 1, focus_generation: 0 };
        let segments = restore(&brain, &plan).unwrap();
        let mut delegated = Delivery::new(&brain, "device", "activation-a", token, "turn".into(), plan.clone(), segments.clone(), vec![], false, 1).unwrap();
        let (message, _, _) = delegated.next(&brain).unwrap().unwrap();
        assert_eq!(message["payload"]["purpose"], "final");
        assert_eq!(message["payload"]["final"], true);
        assert!(delegated.blocks_new_input(true), "the explanation is still on the wire");
        let mut report = message["payload"].clone();
        report["runtime"] = message["runtime"].clone();
        report["status"] = json!("completed");
        assert!(delegated.report(&brain, &report).unwrap(), "no background task is attached, so the foreground ends");
        assert!(!delegated.blocks_new_input(true));

        let mut baseline = Delivery::new(&brain, "device", "activation-b", RuntimeToken { epoch: 2, ..token }, "turn".into(), plan, segments, vec!["task-1".into()], false, 1).unwrap();
        let (message, _, _) = baseline.next(&brain).unwrap().unwrap();
        assert_eq!(message["payload"]["purpose"], "task_ack");
        assert_eq!(message["payload"]["final"], false);
        let mut report = message["payload"].clone();
        report["runtime"] = message["runtime"].clone();
        report["status"] = json!("completed");
        assert!(!baseline.report(&brain, &report).unwrap(), "the baseline keeps the foreground until the task list is empty");
        assert!(baseline.blocks_new_input(false));
        assert!(!baseline.blocks_new_input(true), "with the switch on, a finished explanation does not block the next input");
        drop(brain);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_segment_longer_than_eight_seconds_still_completes() {
        use crate::brain::presentation::{PlanContext, PresentationPlanner};
        let dir = std::env::temp_dir().join(format!("xiaodai-long-{}", uuid::Uuid::new_v4()));
        let brain = Brain::new(dir.clone(), None, Arc::new(|_| {})).unwrap();
        let scene = scene_for("这是一段比较长的回答，设备还在播放。", "thinking");
        let context = PlanContext { household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn".into(), task_id: None };
        let plan = PresentationPlanner::default().freeze(context, &scene, None, false).unwrap();
        brain.tasks.save_plan(&plan).unwrap();
        let mut segments = restore(&brain, &plan).unwrap();
        let pcm = vec![0u8; 10 * 32_000];
        segments[0].audio = Some(json!({"wav_base64": STANDARD.encode(crate::brain::media::pcm_wav(&pcm))}));
        let token = RuntimeToken { epoch: 1, session_id: 1, turn_id: 1, focus_generation: 0 };
        let mut delivery = Delivery::new(&brain, "device", "activation", token, "turn".into(), plan, segments, vec![], true, 1).unwrap();
        let (message, _, _) = delivery.next(&brain).unwrap().unwrap();
        assert!(delivery.report_budget > Duration::from_secs(8), "ten seconds of audio outlasts a fixed eight-second cutoff");
        tokio::time::sleep(Duration::from_secs(9)).await;
        assert!(!delivery.report_expired(), "the segment is still inside its audio budget");
        let mut report = message["payload"].clone();
        report["runtime"] = message["runtime"].clone();
        report["status"] = json!("completed");
        assert!(delivery.report(&brain, &report).is_ok(), "a normal long segment still accepts its receipt");
        drop(brain);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failed_turn_becomes_one_spoken_in_character_segment() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let dir=std::env::temp_dir().join(format!("xiaodai-failure-{}",uuid::Uuid::new_v4()));
        let brain=Brain::new(dir.clone(),None,Arc::new(|_|{})).unwrap();
        let (line,_)=crate::brain::voice_lines::failure_line("asr_no_speech","turn-1").unwrap();
        let wav=json!({"wav_base64":STANDARD.encode(crate::brain::media::pcm_wav(&[0;640])),"duration_ms":20});
        brain.failure_audio.lock().await.insert(line.into(),wav);
        let (plan,segments)=prepare_failure(&brain,"turn-1","asr_no_speech").await.unwrap().expect("spoken");
        assert_eq!(segments.len(),1);
        assert_eq!(segments[0].scene.speech,line);
        assert!(plan.segments[0].speech_asset_id.is_some());
        assert!(prepare_failure(&brain,"turn-1","cancelled").await.unwrap().is_none(),"cancellation stays silent");
        drop(brain);
        let _=std::fs::remove_dir_all(dir);
    }
}
