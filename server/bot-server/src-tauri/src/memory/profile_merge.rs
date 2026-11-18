//! Explicit console-only identity consolidation. Profiles are aliases, never
//! new biometric evidence; original observations and template samples survive.
use std::collections::BTreeSet;
use rusqlite::{params,Connection,OptionalExtension};
use serde_json::{json,Value};
use super::Store;
use super::household::{encoded,parse,revision_changed,uid};
use super::store::{bump_revision,db_error,now_ms,profile_exists,validate_id};

pub(super) fn active_profile(db:&Connection,id:&str)->Result<(),String>{
    profile_exists(db,id)?;
    let merged:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM profile_aliases WHERE source_profile_id=?1)",[id],|r|r.get(0)).map_err(db_error)?;
    if merged{return Err("profile_merged_use_target".into());}Ok(())
}

pub(super) fn canonical_profile(db:&Connection,id:&str)->Result<String,String>{
    profile_exists(db,id)?;
    Ok(db.query_row("SELECT target_profile_id FROM profile_aliases WHERE source_profile_id=?1",[id],|r|r.get(0)).optional().map_err(db_error)?.unwrap_or_else(||id.to_owned()))
}

fn preflight(db:&Connection,source:&str,target:&str)->Result<bool,String>{
    validate_id(source)?;validate_id(target)?;
    if source==target{return Err("profile_merge_requires_distinct_members".into());}
    profile_exists(db,source)?;active_profile(db,target)?;
    let existing:Option<String>=db.query_row("SELECT target_profile_id FROM profile_aliases WHERE source_profile_id=?1",[source],|r|r.get(0)).optional().map_err(db_error)?;
    if let Some(existing)=existing{if existing==target{return Ok(false);}return Err("profile_already_merged".into());}
    Ok(true)
}

fn affected_sources(db:&Connection,source:&str)->Result<Vec<String>,String>{
    let mut q=db.prepare("SELECT DISTINCT t.id FROM household_turns t WHERE t.forgotten=0 AND (t.private_profile_id=?1 OR EXISTS(SELECT 1 FROM household_segments s WHERE s.turn_id=t.id AND (s.automatic_person_id=?1 OR s.annotated_person_id=?1)) OR EXISTS(SELECT 1 FROM household_derived_sources s JOIN household_derived d ON d.id=s.derived_id WHERE s.turn_id=t.id AND (d.private_profile_id=?1 OR EXISTS(SELECT 1 FROM json_each(d.subject_ids_json) WHERE value=?1)))) ORDER BY t.id").map_err(db_error)?;
    let rows=q.query_map([source],|r|r.get::<_,String>(0)).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
}

impl Store {
    /// The host uses this before fencing tasks/assets. It repeats the same
    /// validations inside merge_profile's transaction before changing data.
    pub fn profile_merge_sources(&self,source:&str,target:&str)->Result<Vec<String>,String>{
        let db=self.connection()?;if !preflight(&db,source,target)?{return Ok(Vec::new());}
        let mut keys=super::provenance::household_cleanup_keys(&db,&affected_sources(&db,source)?)?;
        let mut q=db.prepare("SELECT r.source_ref FROM legacy_memory_source_refs r JOIN memories m ON m.id=r.memory_id WHERE m.profile_id=?1 ORDER BY m.id").map_err(db_error)?;
        let rows=q.query_map([source],|r|r.get::<_,String>(0)).map_err(db_error)?;keys.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?);Ok(keys)
    }

    pub fn merge_profile(&self,source:&str,target:&str,guardian_confirmed:bool)->Result<Value,String>{
        if !guardian_confirmed{return Err("guardian_confirmation_required".into());}
        let mut db=self.connection()?;let tx=db.transaction().map_err(db_error)?;
        if !preflight(&tx,source,target)?{return Ok(json!({"merged":true,"idempotent":true,"source_profile_id":source,"target_profile_id":target,"affected_source_turns":[]}));}
        let sources=affected_sources(&tx,source)?;
        let archived:i64=tx.query_row("SELECT count(*) FROM enrollments WHERE profile_id=?1",[source],|r|r.get(0)).map_err(db_error)?;
        tx.execute("UPDATE profiles SET is_child=MAX(is_child,(SELECT is_child FROM profiles WHERE id=?1)),updated_at_ms=?2 WHERE id=?3",params![source,now_ms(),target]).map_err(db_error)?;
        // Keep old conversations and annotation/recognition audit records with
        // their original IDs. Current private notes keep profile scope and the
        // original allow_cloud flags while changing their canonical person.
        tx.execute("UPDATE memories SET profile_id=?1,updated_at_ms=?2 WHERE profile_id=?3",params![target,now_ms(),source]).map_err(db_error)?;
        tx.execute("UPDATE memory_jobs SET profile_id=?1 WHERE profile_id=?2",params![target,source]).map_err(db_error)?;
        tx.execute("UPDATE household_turns SET private_profile_id=?1 WHERE private_profile_id=?2",params![target,source]).map_err(db_error)?;
        tx.execute("UPDATE household_model_conversations SET private_profile_id=?1 WHERE private_profile_id=?2",params![target,source]).map_err(db_error)?;
        tx.execute("UPDATE household_segments SET automatic_person_id=CASE WHEN automatic_person_id=?1 THEN ?2 ELSE automatic_person_id END,annotated_person_id=CASE WHEN annotated_person_id=?1 THEN ?2 ELSE annotated_person_id END,annotation_revision=annotation_revision+1,updated_at_ms=?3 WHERE automatic_person_id=?1 OR annotated_person_id=?1",params![source,target,now_ms()]).map_err(db_error)?;
        // Move review excerpts together while keeping prior enrollment records independent.
        tx.execute("UPDATE voice_clips SET group_annotation_revision=(SELECT annotation_revision FROM household_segments WHERE id=segment_id) WHERE group_annotation_revision IS NOT NULL AND group_id IN(SELECT id FROM voice_groups WHERE person_id IN(?1,?2))",params![source,target]).map_err(db_error)?;
        tx.execute("UPDATE voice_clips SET group_id=(SELECT id FROM voice_groups WHERE person_id=?2 ORDER BY id LIMIT 1) WHERE group_id IN(SELECT id FROM voice_groups WHERE person_id=?1) AND EXISTS(SELECT 1 FROM voice_groups WHERE person_id=?2)",params![source,target]).map_err(db_error)?;
        tx.execute("UPDATE voice_groups SET person_id=?1,revision=revision+1 WHERE person_id=?2",params![target,source]).map_err(db_error)?;
        let derived={let mut q=tx.prepare("SELECT id,episode_id,subject_ids_json FROM household_derived WHERE private_profile_id=?1 OR EXISTS(SELECT 1 FROM json_each(subject_ids_json) WHERE value=?1)").map_err(db_error)?;let rows=q.query_map([source],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
        let mut episodes=BTreeSet::new();
        for (id,episode,raw) in derived{
            episodes.insert(episode);
            let mut subjects:Vec<String>=parse(raw).as_array().into_iter().flatten().filter_map(Value::as_str).map(|id|if id==source{target.to_owned()}else{id.to_owned()}).collect();subjects.sort();subjects.dedup();
            tx.execute("UPDATE household_derived SET subject_ids_json=?1,private_profile_id=CASE WHEN private_profile_id=?2 THEN ?3 ELSE private_profile_id END WHERE id=?4",params![encoded(&json!(subjects),4096)?,source,target,id]).map_err(db_error)?;
        }
        for turn in &sources{
            let episode:String=tx.query_row("SELECT episode_id FROM household_turns WHERE id=?1",[turn],|r|r.get(0)).map_err(db_error)?;episodes.insert(episode);
            super::provenance::queue_household_cleanup(&tx,turn)?;
            tx.execute("UPDATE household_turns SET source_revision=source_revision+1,updated_at_ms=?1 WHERE id=?2",params![now_ms(),turn]).map_err(db_error)?;
            tx.execute("INSERT INTO household_source_revisions(turn_id,revision,input_text,reason,at_ms) SELECT id,source_revision,input_text,'member_merge',?1 FROM household_turns WHERE id=?2 AND source_expired=0 AND input_text!=''",params![now_ms(),turn]).map_err(db_error)?;
        }
        // Flatten aliases without modifying original merge event provenance.
        tx.execute("UPDATE profile_aliases SET target_profile_id=?1 WHERE target_profile_id=?2",params![target,source]).map_err(db_error)?;
        tx.execute("INSERT INTO profile_aliases(source_profile_id,target_profile_id,merged_at_ms) VALUES(?1,?2,?3)",params![source,target,now_ms()]).map_err(db_error)?;
        tx.execute("INSERT INTO profile_merge_events(id,source_profile_id,target_profile_id,merged_at_ms,archived_enrollments) VALUES(?1,?2,?3,?4,?5)",params![uid("merge"),source,target,now_ms(),archived]).map_err(db_error)?;
        let mut retained=Vec::new();
        for episode in &episodes{let mut q=tx.prepare("SELECT d.id FROM household_derived d WHERE d.episode_id=?1 AND d.valid=1 AND d.kind='fact' AND d.confirmed=1 AND EXISTS(SELECT 1 FROM household_derived_sources s JOIN household_turns t ON t.id=s.turn_id WHERE s.derived_id=d.id AND t.source_expired=1 AND t.forgotten=0)").map_err(db_error)?;let rows=q.query_map([episode],|r|r.get::<_,String>(0)).map_err(db_error)?;retained.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?);}
        for episode in episodes{revision_changed(&tx,&episode,false)?;super::household_jobs::queue_organize(&tx,&episode)?;}
        for id in retained{tx.execute("UPDATE household_derived SET valid=1 WHERE id=?1",[id]).map_err(db_error)?;}
        tx.execute("UPDATE households SET policy_revision=policy_revision+1 WHERE singleton=1",[]).map_err(db_error)?;bump_revision(&tx)?;
        tx.commit().map_err(db_error)?;
        Ok(json!({"merged":true,"source_profile_id":source,"target_profile_id":target,"affected_source_turns":sources,"archived_enrollment_count":archived,"biometric_templates":"preserved_inactive","templates_combined":false}))
    }

    /// Audit metadata only; neither centroids nor sample embeddings leave SQLite.
    pub fn profile_aliases(&self)->Result<Value,String>{
        let db=self.connection()?;let mut q=db.prepare("SELECT a.source_profile_id,a.target_profile_id,p.display_name,a.merged_at_ms,(SELECT count(*) FROM enrollments e WHERE e.profile_id=a.source_profile_id) FROM profile_aliases a JOIN profiles p ON p.id=a.source_profile_id ORDER BY a.merged_at_ms").map_err(db_error)?;
        let rows=q.query_map([],|r|Ok(json!({"source_profile_id":r.get::<_,String>(0)?,"target_profile_id":r.get::<_,String>(1)?,"previous_display_name":r.get::<_,String>(2)?,"merged_at_ms":r.get::<_,i64>(3)?,"archived_enrollment_count":r.get::<_,i64>(4)?,"templates_active":false}))).map_err(db_error)?;Ok(json!(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?))
    }
}
