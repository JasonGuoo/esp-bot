//! Durable source-versioned outbox. The database transaction, never a worker
//! response or a model watermark, decides whether an extraction is current.
use std::collections::{BTreeMap, BTreeSet};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use super::{Store, HouseholdExtraction, HouseholdJob};
use super::household::{encoded, parse, uid};
use super::store::{db_error, now_ms};

pub(super) fn digest(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

/// A bounded episode, not the day's whole transcript. Its version incorporates
/// every source revision, including corrections/identity/privacy and receipts.
fn snapshot(db: &Connection, episode: &str) -> Result<Value, String> {
    let (household, day, date, revision): (String, String, String, u64) = db.query_row(
        "SELECT d.household_id,d.id,d.local_date,e.revision FROM household_episodes e JOIN household_days d ON d.id=e.daily_id WHERE e.id=?1", [episode],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(db_error)?;
    let mut query = db.prepare("SELECT id,source_revision,input_text,assistant_text,private_profile_id,allow_cloud,occurred_at_ms,status FROM household_turns WHERE episode_id=?1 AND forgotten=0 AND source_expired=0 AND input_text!='' ORDER BY occurred_at_ms,recorded_at_ms LIMIT 12").map_err(db_error)?;
    let rows = query.query_map([episode], |r| Ok(json!({
        "conversation_turn_id":r.get::<_,String>(0)?,"source_revision":r.get::<_,u64>(1)?,
        "text":r.get::<_,String>(2)?,"assistant_generated":r.get::<_,String>(3)?,
        "private_profile_id":r.get::<_,Option<String>>(4)?,"allow_cloud":r.get::<_,bool>(5)?,
        "occurred_at_ms":r.get::<_,i64>(6)?,"status":r.get::<_,String>(7)?
    }))).map_err(db_error)?;
    let mut sources = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    drop(query);
    // A small prior-day evidence window permits retained_later observations
    // without feeding a whole history back into the model or summarizing a
    // summary. Baselines still cite the original, currently retained source.
    if !sources.is_empty(){
        let mut q=db.prepare("SELECT DISTINCT t.id,t.source_revision,t.input_text,t.assistant_text,t.private_profile_id,t.allow_cloud,t.occurred_at_ms,t.status FROM household_turns t JOIN household_derived_sources s ON s.turn_id=t.id AND s.source_revision=t.source_revision JOIN household_derived d ON d.id=s.derived_id JOIN household_episodes e ON e.id=t.episode_id WHERE d.valid=1 AND d.kind='learning' AND d.evidence IN('correct_with_help','correct_independently','retained_later') AND e.daily_id!=?1 AND t.forgotten=0 AND t.source_expired=0 AND t.input_text!='' AND t.occurred_at_ms<(SELECT opened_at_ms FROM household_episodes WHERE id=?2) ORDER BY t.occurred_at_ms DESC LIMIT 4").map_err(db_error)?;
        let rows=q.query_map(params![day,episode],|r|Ok(json!({"conversation_turn_id":r.get::<_,String>(0)?,"source_revision":r.get::<_,u64>(1)?,"text":r.get::<_,String>(2)?,"assistant_generated":r.get::<_,String>(3)?,"private_profile_id":r.get::<_,Option<String>>(4)?,"allow_cloud":r.get::<_,bool>(5)?,"occurred_at_ms":r.get::<_,i64>(6)?,"status":r.get::<_,String>(7)?,"is_baseline":true}))).map_err(db_error)?;
        sources.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?);
    }
    // This budget covers the entire job, not each turn separately. Large
    // archives must never turn a receipt write into an oversized model request.
    // Full text, all segments and receipts remain in the authoritative journal.
    let text_limit=(6_000/sources.len().max(1)).min(2_000);
    for source in &mut sources {
        let turn = source["conversation_turn_id"].as_str().unwrap_or("").to_owned();
        // The exact version includes a digest of the full input even when the
        // extraction model receives only a bounded, explicitly marked excerpt.
        source["full_text_digest"] = json!(digest(&source["text"]));
        let full = source["text"].as_str().unwrap_or("").to_owned();
        source["text_excerpt"] = json!(full.chars().count() > text_limit);
        source["text"] = json!(full.chars().take(text_limit).collect::<String>());
        source["assistant_generated_digest"]=json!(digest(&source["assistant_generated"]));
        source["assistant_excerpt"]=json!(source["assistant_generated"].as_str().unwrap_or("").chars().count()>120);
        source["assistant_generated"] = json!(source["assistant_generated"].as_str().unwrap_or("").chars().take(120).collect::<String>());
        let mut q = db.prepare("SELECT id,CASE WHEN annotation_set=1 THEN annotated_person_id ELSE automatic_person_id END,annotation_set,overlap,substr(speaker_label,1,64),start_sample,end_sample,sample_rate FROM household_segments WHERE turn_id=?1 ORDER BY annotation_set DESC,start_sample,id LIMIT 9").map_err(db_error)?;
        let rows=q.query_map([&turn],|r|Ok(json!({"speaker_segment_id":r.get::<_,String>(0)?,"person_id":r.get::<_,Option<String>>(1)?,"human_annotated":r.get::<_,bool>(2)?,"overlap":r.get::<_,bool>(3)?,"speaker_label":r.get::<_,Option<String>>(4)?,"start_sample":r.get::<_,Option<i64>>(5)?,"end_sample":r.get::<_,Option<i64>>(6)?,"sample_rate":r.get::<_,Option<i64>>(7)?}))).map_err(db_error)?;
        let mut segments=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        source["segments_excerpt"]=json!(segments.len()>8);segments.truncate(8);source["segments"]=json!(segments);
        let mut q=db.prepare("SELECT event_key,kind,payload_json FROM household_receipts WHERE turn_id=?1 ORDER BY CASE WHEN json_extract(payload_json,'$.status')='completed' THEN 0 ELSE 1 END,at_ms DESC,event_key DESC LIMIT 3").map_err(db_error)?;
        let rows=q.query_map([&turn],|r|{
            let body=parse(r.get(2)?);
            let short=|field:&str,max:usize|body[field].as_str().map(|text|text.chars().take(max).collect::<String>());
            Ok(json!({"event_key":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"payload":{"status":short("status",32),"segment_id":short("segment_id",160),"presentation_id":short("presentation_id",160),"rendered_text":short("rendered_text",192)},"rendered_text_excerpt":body["rendered_text"].as_str().is_some_and(|text|text.chars().count()>192)}))
        }).map_err(db_error)?;
        let mut receipts=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        source["receipts_excerpt"]=json!(receipts.len()>2);receipts.truncate(2);source["actual_receipts"]=json!(receipts);
        let (receipt_count,receipt_version):(i64,i64)=db.query_row("SELECT count(*),COALESCE(max(rowid),0) FROM household_receipts WHERE turn_id=?1",[&turn],|r|Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        source["receipt_count"]=json!(receipt_count);source["receipt_version"]=json!(receipt_version);
    }
    let mut entities=Vec::new();
    {let mut q=db.prepare("SELECT p.id,p.display_name,p.is_child FROM profiles p WHERE NOT EXISTS(SELECT 1 FROM profile_aliases a WHERE a.source_profile_id=p.id) ORDER BY CASE WHEN EXISTS(SELECT 1 FROM household_segments s JOIN household_turns t ON t.id=s.turn_id WHERE t.episode_id=?1 AND (s.automatic_person_id=p.id OR s.annotated_person_id=p.id)) THEN 0 ELSE 1 END,p.id LIMIT 33").map_err(db_error)?;let rows=q.query_map([episode],|r|Ok(json!({"id":r.get::<_,String>(0)?,"display_name":r.get::<_,String>(1)?,"is_child":r.get::<_,bool>(2)?}))).map_err(db_error)?;entities.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?);}
    let entities_excerpt=entities.len()>32;entities.truncate(32);let mut alias_budget=1_024usize;
    for entity in &mut entities{
        let mut q=db.prepare("SELECT p.display_name FROM profile_aliases a JOIN profiles p ON p.id=a.source_profile_id WHERE a.target_profile_id=?1 ORDER BY a.source_profile_id LIMIT 9").map_err(db_error)?;
        let rows=q.query_map([entity["id"].as_str().unwrap_or("")],|r|r.get::<_,String>(0)).map_err(db_error)?;
        let names=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;let mut aliases=Vec::new();let mut excerpt=names.len()>8;
        for name in names.into_iter().take(8){let cost=name.chars().count();if cost>alias_budget{excerpt=true;continue;}alias_budget-=cost;aliases.push(name);}
        entity["aliases"]=json!(aliases);entity["aliases_excerpt"]=json!(excerpt);
    }
    Ok(json!({"schema_version":1,"extractor_version":"household-qwen-json-v2-bounded","household_id":household,"daily_conversation_id":day,"local_date":date,"episode_id":episode,"episode_revision":revision,"text_budget_chars":6000,"sources":sources,"entities":entities,"entities_excerpt":entities_excerpt}))
}

pub(super) fn queue_organize(db: &Connection, episode: &str) -> Result<(), String> {
    let payload = snapshot(db,episode)?;
    if payload["sources"].as_array().map(|v|v.is_empty()).unwrap_or(true) { return Ok(()); }
    let version=digest(&payload);
    db.execute("UPDATE household_jobs SET state='obsolete',lease=NULL,lease_until_ms=NULL,payload_json='{}' WHERE episode_id=?1 AND kind='organize' AND input_version!=?2 AND state IN('queued','running','failed','blocked')",params![episode,version]).map_err(db_error)?;
    // A short quiet period coalesces ASR, late identity, generation and actual
    // receipts. Expired leases remain retryable after process/server crashes.
    db.execute("INSERT INTO household_jobs(id,episode_id,kind,input_version,payload_json,allow_cloud,state,available_at_ms,created_at_ms) VALUES(?1,?2,'organize',?3,?4,0,'queued',?5,?6) ON CONFLICT(episode_id,kind,input_version) DO UPDATE SET state='queued',payload_json=excluded.payload_json,lease=NULL,lease_until_ms=NULL,attempts=0,available_at_ms=excluded.available_at_ms,last_error=NULL WHERE household_jobs.state='obsolete'",params![uid("job"),episode,version,encoded(&payload,524_288)?,now_ms()+3_000,now_ms()]).map_err(db_error)?;
    Ok(())
}

pub(super) fn queue_graph_delete(db:&Connection,episode:&str)->Result<(),String>{
    let old:Option<(String,String)>=db.query_row("SELECT graph_uuid,input_version FROM household_graph_projection WHERE episode_id=?1 AND state!='deleted'",[episode],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    if let Some((uuid,version))=old {
        queue_delete_uuid(db,episode,&uuid,&version)?;
        db.execute("UPDATE household_graph_projection SET state='deleting',updated_at_ms=?1 WHERE episode_id=?2",params![now_ms(),episode]).map_err(db_error)?;
    }
    // An ingest may have written remotely before its process was cancelled.
    // Its preallocated UUID is durable, so cleanup does not depend on a reply.
    let rows={let mut q=db.prepare("SELECT payload_json,input_version FROM household_jobs WHERE episode_id=?1 AND kind='graph_ingest' AND state IN('queued','running','failed','blocked','obsolete')").map_err(db_error)?;let rows=q.query_map([episode],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
    for (body,version) in rows {if let Some(uuid)=parse(body)["graph_uuid"].as_str(){queue_delete_uuid(db,episode,uuid,&version)?;}}
    // revision_changed may already have marked the ingest obsolete before
    // entering this function. Its UUID has now been durably copied to the
    // delete outbox, so clear that obsolete payload as well.
    db.execute("UPDATE household_jobs SET state='obsolete',lease=NULL,lease_until_ms=NULL,payload_json='{}' WHERE episode_id=?1 AND kind='graph_ingest' AND state IN('queued','running','failed','blocked','obsolete')",[episode]).map_err(db_error)?;
    Ok(())
}

fn queue_delete_uuid(db:&Connection,episode:&str,uuid:&str,version:&str)->Result<(),String>{
    let key=format!("{version}:{uuid}");
    // Successful jobs intentionally erase their payload. Re-queueing the same
    // UUID must restore it, otherwise deletion fails forever with uuid_missing
    // and blocks every later ingest for the episode.
    db.execute("INSERT INTO household_jobs(id,episode_id,kind,input_version,payload_json,state,available_at_ms,created_at_ms) VALUES(?1,?2,'graph_delete',?3,?4,'queued',?5,?5) ON CONFLICT(episode_id,kind,input_version) DO UPDATE SET state=CASE WHEN state='running' THEN state ELSE 'queued' END,payload_json=excluded.payload_json,available_at_ms=excluded.available_at_ms,last_error=NULL",params![uid("job"),episode,key,encoded(&json!({"graph_uuid":uuid}),1024)?,now_ms()]).map_err(db_error)?;
    Ok(())
}

/// Graphiti receives only validated, shared derived observations with exact
/// source quotes. Private source bodies never enter the household graph.
fn queue_graph_ingest(db:&Connection,episode:&str,version:&str)->Result<(),String>{
    queue_graph_delete(db,episode)?;
    let snap=snapshot(db,episode)?;
    if digest(&snap)!=version {return Ok(());}
    let rows={let mut q=db.prepare("SELECT id,kind,content,sources_json,subject_ids_json,topic_id,evidence,assertion_kind,allow_cloud FROM household_derived WHERE episode_id=?1 AND input_version=?2 AND valid=1 AND private_profile_id IS NULL ORDER BY id LIMIT 24").map_err(db_error)?;
        let rows=q.query_map(params![episode,version],|r|Ok(json!({"id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"content":r.get::<_,String>(2)?,"sources":parse(r.get(3)?),"subject_entity_ids":parse(r.get(4)?),"topic_id":r.get::<_,Option<String>>(5)?,"evidence":r.get::<_,Option<String>>(6)?,"assertion_kind":r.get::<_,String>(7)?,"allow_cloud":r.get::<_,bool>(8)?}))).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
    if rows.is_empty(){return Ok(());}
    let allow_cloud=rows.iter().all(|item|item["allow_cloud"].as_bool()==Some(true));
    let uuid=uuid::Uuid::new_v4().to_string();
    let reference_time=snap["sources"].as_array().and_then(|s|s.first()).map(|s|s["occurred_at_ms"].clone()).unwrap_or(json!(now_ms()));
    let payload=json!({"graph_uuid":uuid,"household_id":snap["household_id"],"daily_conversation_id":snap["daily_conversation_id"],"local_date":snap["local_date"],"episode_id":episode,"input_version":version,"reference_time_ms":reference_time,"items":rows,"allow_cloud":allow_cloud});
    db.execute("INSERT INTO household_graph_projection(episode_id,graph_uuid,input_version,state,updated_at_ms) VALUES(?1,?2,?3,'pending',?4) ON CONFLICT(episode_id) DO UPDATE SET graph_uuid=excluded.graph_uuid,input_version=excluded.input_version,state='pending',updated_at_ms=excluded.updated_at_ms",params![episode,uuid,version,now_ms()]).map_err(db_error)?;
    db.execute("INSERT INTO household_jobs(id,episode_id,kind,input_version,payload_json,allow_cloud,state,available_at_ms,created_at_ms) VALUES(?1,?2,'graph_ingest',?3,?4,?5,'queued',?6,?6) ON CONFLICT(episode_id,kind,input_version) DO UPDATE SET payload_json=excluded.payload_json,allow_cloud=excluded.allow_cloud,state='queued',lease=NULL,lease_until_ms=NULL,attempts=0,available_at_ms=excluded.available_at_ms,last_error=NULL",params![uid("job"),episode,version,encoded(&payload,262_144)?,allow_cloud,now_ms()]).map_err(db_error)?;
    Ok(())
}

impl Store {
    pub fn next_household_job(&self,graph:bool)->Result<Option<HouseholdJob>,String>{
        self.purge_household_expired()?;
        let mut db=self.connection()?;let tx=db.transaction().map_err(db_error)?;let now=now_ms();
        if graph {
            // Expand at most one rebuild marker per idle iteration. A model
            // configuration change must not materialize a year's snapshots on
            // the foreground recall path or hold the SQLite lock that long.
            let pending:Option<(String,Option<String>)>=tx.query_row("SELECT r.episode_id,e.processed_version FROM household_graph_rebuild r JOIN household_episodes e ON e.id=r.episode_id ORDER BY r.requested_at_ms LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
            if let Some((episode,version))=pending{
                queue_graph_delete(&tx,&episode)?;
                if let Some(version)=version{queue_graph_ingest(&tx,&episode,&version)?;}
                queue_organize(&tx,&episode)?;
                tx.execute("DELETE FROM household_graph_rebuild WHERE episode_id=?1",[episode]).map_err(db_error)?;
            }
        }
        tx.execute("UPDATE household_jobs SET state='queued',lease=NULL,lease_until_ms=NULL,last_error='worker_lease_expired' WHERE state='running' AND lease_until_ms<=?1",[now]).map_err(db_error)?;
        let row:Option<(String,String,String,String,String,bool,String)>=tx.query_row("SELECT j.id,j.kind,j.episode_id,j.input_version,j.payload_json,j.allow_cloud,d.household_id FROM household_jobs j JOIN household_episodes e ON e.id=j.episode_id JOIN household_days d ON d.id=e.daily_id WHERE j.state='queued' AND j.available_at_ms<=?1 AND ((?2=0 AND j.kind='organize') OR (?2=1 AND j.kind IN('graph_delete','graph_ingest'))) AND (j.kind!='graph_ingest' OR NOT EXISTS(SELECT 1 FROM household_jobs q WHERE q.episode_id=j.episode_id AND q.kind='graph_delete' AND q.state IN('queued','running','failed','blocked'))) ORDER BY CASE j.kind WHEN 'graph_delete' THEN 0 ELSE 1 END,j.created_at_ms LIMIT 1",params![now,graph],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional().map_err(db_error)?;
        let Some((id,kind,episode,input_version,payload,allow_cloud,household))=row else{tx.commit().map_err(db_error)?;return Ok(None)};
        let lease=uid("lease");
        tx.execute("UPDATE household_jobs SET state='running',lease=?1,lease_until_ms=?2 WHERE id=?3",params![lease,now+240_000,id]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(Some(HouseholdJob{id,lease,kind,household_id:household,episode_id:episode,input_version,payload:parse(payload),allow_cloud}))
    }

    pub fn release_household_job(&self,job:&HouseholdJob)->Result<(),String>{
        self.connection()?.execute("UPDATE household_jobs SET state='queued',lease=NULL,lease_until_ms=NULL,available_at_ms=?1 WHERE id=?2 AND lease=?3 AND state='running'",params![now_ms()+3_000,job.id,job.lease]).map_err(db_error)?;Ok(())
    }

    pub fn fail_household_job(&self,job:&HouseholdJob,code:&str,blocked:bool)->Result<(),String>{
        let safe:String=code.chars().filter(|c|c.is_ascii_alphanumeric()||*c=='_').take(80).collect();
        self.connection()?.execute("UPDATE household_jobs SET attempts=attempts+1,state=CASE WHEN ?1=1 THEN 'blocked' WHEN attempts>=2 THEN 'failed' ELSE 'queued' END,lease=NULL,lease_until_ms=NULL,available_at_ms=?2+(attempts+1)*30000,last_error=?3 WHERE id=?4 AND lease=?5 AND state='running'",params![blocked,now_ms(),safe,job.id,job.lease]).map_err(db_error)?;Ok(())
    }

    pub fn complete_household_job(&self,job:&HouseholdJob,result:&HouseholdExtraction)->Result<bool,String>{
        let mut db=self.connection()?;let tx=db.transaction().map_err(db_error)?;
        if job.kind!="organize"||!job_current(&tx,job)? {return Ok(false);}
        let snap=snapshot(&tx,&job.episode_id)?;
        if digest(&snap)!=job.input_version {obsolete(&tx,job)?;queue_organize(&tx,&job.episode_id)?;tx.commit().map_err(db_error)?;return Ok(false);}
        let sources:BTreeMap<String,Value>=snap["sources"].as_array().into_iter().flatten().map(|s|(s["conversation_turn_id"].as_str().unwrap_or("").to_owned(),s.clone())).collect();
        let previous: BTreeMap<String,bool>={let mut q=tx.prepare("SELECT kind,content,subject_ids_json,sources_json,confirmed FROM household_derived WHERE episode_id=?1 AND valid=1").map_err(db_error)?;let rows=q.query_map([&job.episode_id],|r|Ok((digest(&json!([r.get::<_,String>(0)?,r.get::<_,String>(1)?,parse(r.get(2)?),parse(r.get(3)?)])),r.get::<_,bool>(4)?))).map_err(db_error)?;rows.collect::<rusqlite::Result<BTreeMap<_,_>>>().map_err(db_error)?};
        // Keep only independently confirmed facts whose original source has
        // expired; they cannot be regenerated from an absent transcript.
        tx.execute("DELETE FROM household_derived WHERE episode_id=?1 AND NOT(valid=1 AND kind='fact' AND confirmed=1 AND EXISTS(SELECT 1 FROM household_derived_sources s JOIN household_turns t ON t.id=s.turn_id WHERE s.derived_id=household_derived.id AND t.source_expired=1 AND t.forgotten=0))",[&job.episode_id]).map_err(db_error)?;
        for item in result.items.iter().take(24) {
            if !["event","topic","learning","fact"].contains(&item.kind.as_str())||item.content.trim().is_empty()||item.content.chars().count()>1_000||item.sources.is_empty()||item.sources.len()>6 {continue;}
            if !item.sources.iter().any(|q|sources.get(&q.conversation_turn_id).is_some_and(|s|s["is_baseline"].as_bool()!=Some(true))){continue;}
            if item.kind=="learning"&&!item.evidence.as_deref().is_some_and(|s|["introduced","self_reported","correct_with_help","correct_independently","retained_later"].contains(&s)){continue;}
            let mut refs=Vec::new();let mut owners=BTreeSet::new();let mut allowed=true;let mut valid=true;let mut seen=BTreeSet::new();
            for quote in &item.sources {
                let Some(source)=sources.get(&quote.conversation_turn_id) else{valid=false;break};
                let suppressed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM household_tombstones WHERE (scope='derived_source' AND target_id=?1) OR (scope='derived_episode' AND target_id=?2))",params![quote.conversation_turn_id,job.episode_id],|r|r.get(0)).map_err(db_error)?;
                if suppressed||source["source_revision"].as_u64()!=Some(quote.source_revision)||!seen.insert(quote.conversation_turn_id.clone()) {valid=false;break;}
                let text=source["text"].as_str().unwrap_or("");
                let Some(start)=text.find(&quote.quote).filter(|_|(1..=500).contains(&quote.quote.chars().count())) else{valid=false;break};
                if let Some(segment)=quote.speaker_segment_id.as_deref(){if !source["segments"].as_array().into_iter().flatten().any(|s|s["speaker_segment_id"].as_str()==Some(segment)){valid=false;break;}}
                if let Some(owner)=source["private_profile_id"].as_str(){owners.insert(owner.to_owned());}
                allowed &= source["allow_cloud"].as_bool()==Some(true);
                refs.push(json!({"conversation_turn_id":quote.conversation_turn_id,"source_revision":quote.source_revision,"speaker_segment_id":quote.speaker_segment_id,"quote":quote.quote,"span_start":text[..start].chars().count(),"span_end":text[..start].chars().count()+quote.quote.chars().count(),"span_unit":"unicode_scalar","source_expired":false,"occurred_at_ms":source["occurred_at_ms"]}));
            }
            if !valid||owners.len()>1 {continue;}
            if item.kind=="learning"&&!valid_learning(item,&sources){continue;}
            // These are evidence levels, not a mastery score. Higher claims
            // require multiple user observations and a known subject; models
            // cannot promote an unexplained single exchange to retained_later.
            if item.evidence.as_deref()==Some("retained_later") {
                let times:Vec<i64>=item.sources.iter().filter_map(|q|sources.get(&q.conversation_turn_id)?.get("occurred_at_ms")?.as_i64()).collect();
                if times.len()<2||times.iter().max().unwrap_or(&0)-times.iter().min().unwrap_or(&0)<86_400_000 {continue;}
            }
            let mut subjects=Vec::new();let mut subjects_valid=true;
            for id in &item.subject_entity_ids{match super::profile_merge::canonical_profile(&tx,id){Ok(id)=>subjects.push(id),Err(_)=>{subjects_valid=false;break;}}}
            subjects.sort();subjects.dedup();
            if !subjects_valid||subjects.len()>16{continue;}
            if item.kind=="learning" && item.evidence.as_deref()!=Some("introduced") && subjects.is_empty(){continue;}
            let assertion=item.assertion_kind.as_deref().filter(|s|["observed","self_reported","reported_by_other","uncertain"].contains(s)).unwrap_or("uncertain");
            // A model can describe an anonymous or third-person observation
            // as self-reported. Without a known subject, preserve uncertainty
            // instead of silently assigning the claim to the speaker.
            let assertion=if assertion=="self_reported" && subjects.is_empty() {"uncertain"} else {assertion};
            let topic=item.topic_key.as_deref().map(str::trim).filter(|s|!s.is_empty()&&s.chars().count()<=100).map(|s|format!("topic_{}",&digest(&json!([job.household_id,s.to_lowercase()]))[..24]));
            let id=uid("derived");let content=item.content.trim();let owner=owners.iter().next();
            let confirmation_key=digest(&json!([item.kind,content,subjects,refs]));
            tx.execute("INSERT INTO household_derived(id,episode_id,input_version,kind,content,subject_ids_json,topic_id,evidence,assertion_kind,sources_json,confirmed,allow_cloud,private_profile_id,created_at_ms,learning_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![id,job.episode_id,job.input_version,item.kind,content,encoded(&json!(subjects),4096)?,topic,item.evidence,assertion,encoded(&json!(refs),32768)?,previous.get(&confirmation_key).copied().unwrap_or(false),allowed,owner,now_ms(),encoded(&json!(item.learning),16384)?]).map_err(db_error)?;
            for quote in &item.sources{tx.execute("INSERT INTO household_derived_sources(derived_id,turn_id,source_revision) VALUES(?1,?2,?3)",params![id,quote.conversation_turn_id,quote.source_revision]).map_err(db_error)?;}
        }
        queue_graph_ingest(&tx,&job.episode_id,&job.input_version)?;
        tx.execute("UPDATE household_episodes SET processed_version=?1 WHERE id=?2",params![job.input_version,job.episode_id]).map_err(db_error)?;
        tx.execute("UPDATE household_jobs SET state='done',lease=NULL,lease_until_ms=NULL,payload_json='{}',last_error=NULL WHERE id=?1 AND lease=?2",params![job.id,job.lease]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;Ok(true)
    }

    pub fn complete_household_graph_job(&self,job:&HouseholdJob,result:&Value)->Result<bool,String>{
        let mut db=self.connection()?;let tx=db.transaction().map_err(db_error)?;
        let uuid=job.payload["graph_uuid"].as_str().ok_or("graph_uuid_missing")?;
        let signature=result["engine_signature"].as_str().filter(|v|v.len()==64&&v.bytes().all(|b|b.is_ascii_hexdigit())).ok_or("graph_signature_missing")?;
        let expected:Option<String>=tx.query_row("SELECT graph_engine_signature FROM households WHERE singleton=1",[],|r|r.get(0)).map_err(db_error)?;
        if job.kind=="graph_ingest"&&expected.as_deref()!=Some(signature){queue_delete_uuid(&tx,&job.episode_id,uuid,&job.input_version)?;obsolete(&tx,job)?;tx.commit().map_err(db_error)?;return Ok(false);}
        if job.kind=="graph_ingest"&&(!job_current(&tx,job)?||digest(&snapshot(&tx,&job.episode_id)?)!=job.input_version){
            queue_delete_uuid(&tx,&job.episode_id,uuid,&job.input_version)?;obsolete(&tx,job)?;tx.commit().map_err(db_error)?;return Ok(false);
        }
        if !job_current(&tx,job)? {return Ok(false);}
        match job.kind.as_str(){
            "graph_ingest"=>{tx.execute("UPDATE household_graph_projection SET state='ready',engine_signature=?1,updated_at_ms=?2 WHERE episode_id=?3 AND graph_uuid=?4 AND input_version=?5",params![signature,now_ms(),job.episode_id,uuid,job.input_version]).map_err(db_error)?;tx.execute("UPDATE household_episodes SET indexed_version=?1 WHERE id=?2",params![job.input_version,job.episode_id]).map_err(db_error)?;},
            "graph_delete"=>{tx.execute("UPDATE household_graph_projection SET state='deleted',updated_at_ms=?1 WHERE episode_id=?2 AND graph_uuid=?3 AND state='deleting'",params![now_ms(),job.episode_id,uuid]).map_err(db_error)?;},
            _=>return Err("invalid_graph_job".into()),
        }
        tx.execute("UPDATE household_jobs SET state='done',lease=NULL,lease_until_ms=NULL,payload_json='{}',last_error=NULL WHERE id=?1 AND lease=?2",params![job.id,job.lease]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;Ok(true)
    }

    pub fn household_jobs_status(&self)->Result<Value,String>{
        let db=self.connection()?;let mut q=db.prepare("SELECT id,episode_id,kind,input_version,state,attempts,last_error,available_at_ms FROM household_jobs WHERE state NOT IN('done','obsolete') ORDER BY created_at_ms LIMIT 200").map_err(db_error)?;
        let rows=q.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"episode_id":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"input_version":r.get::<_,String>(3)?,"state":r.get::<_,String>(4)?,"attempts":r.get::<_,i64>(5)?,"error":r.get::<_,Option<String>>(6)?,"available_at_ms":r.get::<_,i64>(7)?}))).map_err(db_error)?;
        let values=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;drop(q);
        let cleanup:i64=db.query_row("SELECT count(*) FROM household_source_cleanup WHERE completed_at_ms IS NULL",[],|r|r.get(0)).map_err(db_error)?;
        let rebuild:i64=db.query_row("SELECT count(*) FROM household_graph_rebuild",[],|r|r.get(0)).map_err(db_error)?;
        Ok(json!({"jobs":values,"source_cleanup_pending":cleanup,"graph_rebuild_pending":rebuild,"graphiti_enabled":self.config.graphiti.enabled,"raw_audio_retained":false}))
    }

    pub fn retry_household_jobs(&self)->Result<(),String>{
        self.connection()?.execute("UPDATE household_jobs SET state='queued',attempts=0,available_at_ms=?1,last_error=NULL WHERE state IN('failed','blocked')",[now_ms()]).map_err(db_error)?;Ok(())
    }

    pub fn rebuild_household_graph(&self)->Result<(),String>{
        self.purge_household_expired()?;let mut db=self.connection()?;let tx=db.transaction().map_err(db_error)?;
        tx.execute("INSERT OR REPLACE INTO household_graph_rebuild(episode_id,requested_at_ms) SELECT id,?1 FROM household_episodes",[now_ms()]).map_err(db_error)?;
        tx.execute("UPDATE household_graph_projection SET state='rebuild_required' WHERE state!='deleted'",[]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }

    pub fn set_graphiti_signature(&self,signature:&str)->Result<bool,String>{
        if signature.len()!=64||!signature.bytes().all(|b|b.is_ascii_hexdigit()){return Err("invalid_graph_signature".into());}
        let mut db=self.connection()?;let tx=db.transaction().map_err(db_error)?;
        let previous:Option<String>=tx.query_row("SELECT graph_engine_signature FROM households WHERE singleton=1",[],|r|r.get(0)).map_err(db_error)?;
        if previous.as_deref()==Some(signature){return Ok(false);}
        tx.execute("UPDATE households SET graph_engine_signature=?1 WHERE singleton=1",[signature]).map_err(db_error)?;
        tx.execute("INSERT OR REPLACE INTO household_graph_rebuild(episode_id,requested_at_ms) SELECT id,?1 FROM household_episodes WHERE processed_version IS NOT NULL",[now_ms()]).map_err(db_error)?;
        tx.execute("UPDATE household_graph_projection SET state='rebuild_required' WHERE state!='deleted'",[]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;Ok(true)
    }

    /// Reject every graph edge whose supporting episodes are stale, deleted,
    /// private or barred from the requested destination. A graph UUID is a
    /// lookup key, never sufficient authority by itself.
    pub fn validate_graph_recall(&self,result:&Value,cloud:bool)->Result<Value,String>{
        self.purge_household_expired()?;let db=self.connection()?;let mut hits=Vec::new();
        let signature=result["engine_signature"].as_str().ok_or("graph_signature_missing")?;
        for edge in result["edges"].as_array().into_iter().flatten().take(24){
            let Some(episodes)=edge["episodes"].as_array().filter(|v|!v.is_empty()) else{continue};
            let mut refs=Vec::new();let mut valid=true;
            for graph_id in episodes.iter().take(32){let Some(uuid)=graph_id.as_str()else{valid=false;break};
                let projection:Option<(String,String)>=db.query_row("SELECT episode_id,input_version FROM household_graph_projection WHERE graph_uuid=?1 AND state='ready' AND engine_signature=?2",params![uuid,signature],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
                let Some((episode,version))=projection else{valid=false;break};
                let forbidden:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM household_derived WHERE episode_id=?1 AND input_version=?2 AND valid=1 AND private_profile_id IS NULL AND ?3=1 AND allow_cloud=0)",params![episode,version,cloud],|r|r.get(0)).map_err(db_error)?;
                if forbidden{valid=false;break;}
                // Validate the sources actually indexed, not today's dynamic
                // baseline selection or newly appended unrelated sources.
                let mut q=db.prepare("SELECT DISTINCT s.turn_id,s.source_revision,t.source_revision,t.source_expired,t.forgotten,t.private_profile_id,t.allow_cloud FROM household_derived d JOIN household_derived_sources s ON s.derived_id=d.id JOIN household_turns t ON t.id=s.turn_id WHERE d.episode_id=?1 AND d.input_version=?2 AND d.valid=1 AND d.private_profile_id IS NULL").map_err(db_error)?;
                let rows=q.query_map(params![episode,version],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,u64>(2)?,r.get::<_,bool>(3)?,r.get::<_,bool>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,bool>(6)?))).map_err(db_error)?;
                let sources=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
                if sources.is_empty()||sources.iter().any(|s|s.1!=s.2||s.3||s.4||s.5.is_some()||(cloud&&!s.6)){valid=false;break;}
                let source_turns:Vec<Value>=sources.into_iter().map(|s|json!({"conversation_turn_id":s.0,"source_revision":s.1})).collect();
                refs.push(json!({"episode_id":episode,"input_version":version,"graph_uuid":uuid,"source_turns":source_turns}));
            }
            if valid&&episodes.len()<=32 {if let Some(fact)=edge["fact"].as_str().filter(|s|s.chars().count()<=1_000){hits.push(json!({"fact":fact,"sources":refs,"source_type":"graphiti_derived_observation"}));}}
        }Ok(json!({"hits":hits}))
    }
}

fn job_current(db:&Connection,job:&HouseholdJob)->Result<bool,String>{db.query_row("SELECT EXISTS(SELECT 1 FROM household_jobs WHERE id=?1 AND lease=?2 AND state='running' AND input_version=?3)",params![job.id,job.lease,job.input_version],|r|r.get(0)).map_err(db_error)}
fn obsolete(db:&Connection,job:&HouseholdJob)->Result<(),String>{db.execute("UPDATE household_jobs SET state='obsolete',lease=NULL,lease_until_ms=NULL,payload_json='{}' WHERE id=?1 AND lease=?2",params![job.id,job.lease]).map_err(db_error)?;Ok(())}

fn valid_learning(item:&super::HouseholdItem,sources:&BTreeMap<String,Value>)->bool{
    let advanced=matches!(item.evidence.as_deref(),Some("correct_with_help"|"correct_independently"|"retained_later"));
    let Some(evidence)=item.learning.as_ref()else{return !advanced};
    if evidence.knowledge_point.is_empty()||evidence.knowledge_point.chars().count()>100||evidence.question.chars().count()>1_000||evidence.answer.chars().count()>1_000||!["none","provided","unknown"].contains(&evidence.hint_level.as_str()){return false;}
    if !advanced{return true;}
    for id in [Some(&evidence.question_source_id),Some(&evidence.answer_source_id),evidence.hint_source_id.as_ref(),evidence.prior_answer_source_id.as_ref()].into_iter().flatten(){
        if !item.sources.iter().any(|source|source.conversation_turn_id==*id){return false;}
    }
    let Some(question)=sources.get(&evidence.question_source_id)else{return false};
    let Some(answer)=sources.get(&evidence.answer_source_id)else{return false};
    if evidence.question.is_empty()||evidence.answer.is_empty()||!answer["text"].as_str().unwrap_or("").contains(&evidence.answer)||!item.sources.iter().any(|s|s.conversation_turn_id==evidence.answer_source_id){return false;}
    if question["occurred_at_ms"].as_i64()>answer["occurred_at_ms"].as_i64(){return false;}
    let question_in_user=question["text"].as_str().unwrap_or("").contains(&evidence.question);
    let question_delivered=question["assistant_generated"].as_str().unwrap_or("").contains(&evidence.question)
        && evidence.question_source_id!=evidence.answer_source_id
        && evidence.question_receipt_key.as_ref().is_some_and(|key|question["actual_receipts"].as_array().into_iter().flatten().any(|r|r["event_key"].as_str()==Some(key.as_str())&&r["kind"]=="presentation"&&r["payload"]["status"]=="completed"&&r["payload"]["rendered_text"].as_str().unwrap_or("").contains(&evidence.question)));
    if !question_in_user&&!question_delivered{return false;}
    if item.evidence.as_deref()==Some("correct_with_help"){
        if evidence.hint_level!="provided"{return false;}
        let Some(hint)=evidence.hint_source_id.as_ref().and_then(|id|sources.get(id))else{return false};
        let Some(quote)=evidence.hint_quote.as_deref().filter(|s|!s.is_empty())else{return false};
        if !hint["text"].as_str().unwrap_or("").contains(quote){return false;}
    }else if evidence.hint_level!="none"{return false;}
    if item.evidence.as_deref()==Some("retained_later"){
        let Some(prior)=evidence.prior_answer_source_id.as_ref().and_then(|id|sources.get(id))else{return false};
        if prior["is_baseline"]!=true||answer["occurred_at_ms"].as_i64().unwrap_or(0)-prior["occurred_at_ms"].as_i64().unwrap_or(0)<86_400_000{return false;}
    }
    true
}
