//! Versioned dependencies connect retained sources to tasks and media without
//! making a correction permanently ban all future uses of the same source.
use std::collections::BTreeSet;
use rusqlite::{params, Connection};
use super::Store;
use super::store::{db_error, now_ms, validate_id};

pub fn household_source_ref(turn: &str, revision: u64) -> Result<String, String> {
    validate_id(turn)?;
    if revision == 0 { return Err("invalid_source_revision".into()); }
    let key = format!("source-ref:{turn}:v{revision}");
    validate_id(&key)?;
    Ok(key)
}

/// Return the canonical turn (its original output) plus every dependency
/// version already issued for it. Automatic identity updates may have advanced
/// the revision since an older conversation consumed the source.
pub(super) fn household_cleanup_keys(db: &Connection, turns: &[String]) -> Result<Vec<String>, String> {
    let mut keys = BTreeSet::new();
    let mut query = db.prepare("SELECT source_ref FROM household_source_ref_versions WHERE turn_id=?1 ORDER BY revision").map_err(db_error)?;
    for turn in turns {
        validate_id(turn)?;
        keys.insert(turn.clone());
        let rows = query.query_map([turn], |r| r.get::<_, String>(0)).map_err(db_error)?;
        keys.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?);
        let mut memories=db.prepare("SELECT d.source_ref FROM household_derived d JOIN household_derived_sources s ON s.derived_id=d.id WHERE s.turn_id=?1").map_err(db_error)?;
        let rows=memories.query_map([turn],|r|r.get::<_,String>(0)).map_err(db_error)?;
        keys.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?);
    }
    Ok(keys.into_iter().collect())
}

pub(super) fn queue_household_cleanup(db: &Connection, turn: &str) -> Result<(), String> {
    let revision: u64 = db.query_row("SELECT source_revision FROM household_turns WHERE id=?1", [turn], |r| r.get(0)).map_err(db_error)?;
    let keys = household_cleanup_keys(db, &[turn.to_owned()])?;
    for key in keys {
        db.execute("INSERT INTO household_source_cleanup(turn_id,source_revision,queued_at_ms) VALUES(?1,?2,?3) ON CONFLICT(turn_id) DO UPDATE SET source_revision=excluded.source_revision,queued_at_ms=excluded.queued_at_ms,completed_at_ms=NULL", params![key,revision,now_ms()]).map_err(db_error)?;
    }
    Ok(())
}

impl Store {
    /// Metadata dependencies for a native conversation, including sources older
    /// than its recent twelve-turn text projection. Never return another
    /// person's private source. No silent truncation is allowed.
    pub fn household_day_source_refs(&self, daily: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
        validate_id(daily)?;
        if let Some(profile) = profile { validate_id(profile)?; }
        self.purge_household_expired()?;
        let db = self.connection()?;
        if let Some(profile) = profile { super::profile_merge::active_profile(&db, profile)?; }
        let mut query = db.prepare("SELECT t.id,t.source_revision FROM household_turns t JOIN household_episodes e ON e.id=t.episode_id WHERE e.daily_id=?1 AND t.forgotten=0 AND t.source_expired=0 AND (t.private_profile_id IS NULL OR t.private_profile_id=?2) AND NOT EXISTS(SELECT 1 FROM household_tombstones x WHERE x.scope='derived_source' AND x.target_id=t.id) ORDER BY t.id LIMIT 2049").map_err(db_error)?;
        let rows = query.query_map(params![daily,profile], |r| Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?))).map_err(db_error)?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        if rows.len() > 2048 { return Err("household_day_source_dependency_limit".into()); }
        rows.into_iter().map(|(id,revision)|household_source_ref(&id,revision)).collect()
    }

    /// Conservative provenance for an existing native context that may contain
    /// previously recalled legacy notes. Scope and cloud permissions match
    /// Store::context; IDs alone do not expose note text. The historical method
    /// name also covers independently retained household facts: native context
    /// may have recalled them before its current query changed.
    pub fn legacy_context_source_refs(&self, profile: Option<&str>, allow_cloud: bool) -> Result<Vec<String>, String> {
        if let Some(profile) = profile { validate_id(profile)?; }
        self.purge_expired()?;
        let db = self.connection()?;
        if let Some(profile) = profile { super::profile_merge::active_profile(&db, profile)?; }
        let mut query = db.prepare("SELECT r.source_ref FROM memories m JOIN legacy_memory_source_refs r ON r.memory_id=m.id WHERE m.confirmed=1 AND (m.expires_at_ms IS NULL OR m.expires_at_ms>?1) AND (m.scope='household' OR (m.scope='profile' AND m.profile_id=?2)) AND (?3=0 OR m.allow_cloud=1) ORDER BY m.id LIMIT 2049").map_err(db_error)?;
        let rows = query.query_map(params![now_ms(),profile,allow_cloud], |r| r.get::<_,String>(0)).map_err(db_error)?;
        let mut refs = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        if refs.len() > 2048 { return Err("persistent_source_dependency_limit".into()); }
        drop(query);
        let mut query=db.prepare("SELECT d.source_ref FROM household_derived d WHERE d.valid=1 AND d.kind='fact' AND d.confirmed=1 AND (d.private_profile_id IS NULL OR d.private_profile_id=?1) AND (?2=0 OR d.allow_cloud=1) AND EXISTS(SELECT 1 FROM json_each(d.sources_json) s WHERE json_extract(s.value,'$.source_expired')=1) ORDER BY d.id LIMIT 2049").map_err(db_error)?;
        let rows=query.query_map(params![profile,allow_cloud],|r|r.get::<_,String>(0)).map_err(db_error)?;
        refs.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?);
        if refs.len()>2048{return Err("persistent_source_dependency_limit".into());}
        Ok(refs)
    }
}
