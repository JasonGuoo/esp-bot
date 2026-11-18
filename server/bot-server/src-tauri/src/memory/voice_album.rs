//! Household speaker discovery. Per-recording diarization labels are never
//! compared across recordings. Membership is derived from retained clean samples.
use super::{
    household::annotate_segment,
    identity::{checked_model_id, cosine, decode, encode, normalize},
    store::{bump_revision, db_error, insert_profile, now_ms},
    NewProfile, Store,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MAX_CLIPS: usize = 256;
pub(super) const MAX_AGE_MS: i64 = 30 * 86_400_000;

pub(crate) struct VoiceSample {
    pub index: usize,
    pub model_id: String,
    pub embedding: Vec<f32>,
    pub wav: Vec<u8>,
    pub duration_ms: u32,
}

fn new_group(db: &Connection, person: Option<&str>) -> Result<i64, String> {
    db.execute("INSERT INTO voice_groups(person_id) VALUES(?1)", [person])
        .map_err(db_error)?;
    Ok(db.last_insert_rowid())
}
fn person_group(db: &Connection, person: &str) -> Result<Option<i64>, String> {
    db.query_row(
        "SELECT id FROM voice_groups WHERE person_id=?1 ORDER BY id LIMIT 1",
        [person],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
}

fn nearest_group(
    db: &Connection,
    turn: &str,
    label: &str,
    model: &str,
    vector: &[f32],
    floor: f32,
    margin: f32,
) -> Result<Option<i64>, String> {
    let mut query = db
        .prepare(
            "SELECT c.group_id,c.embedding FROM voice_clips c WHERE c.model_id=?1
        AND NOT EXISTS(SELECT 1 FROM voice_clips c2 JOIN household_segments s ON s.id=c2.segment_id
            WHERE c2.group_id=c.group_id AND s.turn_id=?2 AND s.speaker_label!=?3)",
        )
        .map_err(db_error)?;
    let rows = query
        .query_map(params![model, turn, label], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .map_err(db_error)?;
    let mut sums: BTreeMap<i64, Vec<f32>> = BTreeMap::new();
    for row in rows {
        let (id, bytes) = row.map_err(db_error)?;
        if bytes.len() != vector.len() * 4 {
            continue;
        }
        let sample = decode(&bytes, vector.len())?;
        let sum = sums.entry(id).or_insert_with(|| vec![0.; vector.len()]);
        for (acc, value) in sum.iter_mut().zip(sample) {
            *acc += value;
        }
    }
    let mut ranked = Vec::new();
    for (id, sum) in sums {
        if let Ok(centroid) = normalize(&sum) {
            ranked.push((id, cosine(vector, &centroid)));
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(ranked
        .first()
        .filter(|(_, score)| {
            *score >= floor
                && ranked
                    .get(1)
                    .is_none_or(|runner| *score - runner.1 >= margin)
        })
        .map(|(id, _)| *id))
}

pub(super) fn expire(db: &Connection) -> Result<(), String> {
    db.execute("DELETE FROM voice_clips WHERE segment_id IN(SELECT s.id FROM household_segments s JOIN household_turns t ON s.turn_id=t.id WHERE t.occurred_at_ms<=?1)",[now_ms()-MAX_AGE_MS]).map_err(db_error)?;
    Ok(())
}

pub(super) fn collect_samples(
    db: &Connection,
    turn: &str,
    segments: &[Value],
    samples: &[VoiceSample],
    floor: f32,
    margin: f32,
) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }
    let eligible:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM household_turns t CROSS JOIN households h
        WHERE t.id=?1 AND t.private_profile_id IS NULL AND t.forgotten=0 AND t.source_expired=0 AND t.input_kind='audio'
        AND h.voice_review_enabled=1 AND h.voice_review_generation=t.voice_review_generation AND t.occurred_at_ms>?2)",
        params![turn,now_ms()-MAX_AGE_MS],|r|r.get(0)).map_err(db_error)?;
    if !eligible {
        return Ok(());
    }
    expire(db)?;
    for sample in samples {
        let Some(segment) = segments.get(sample.index) else {
            continue;
        };
        let Some(label) = segment["speaker_label"].as_str() else {
            continue;
        };
        if segment["single_speaker_verified"] != true
            || segment["overlap"] != false
            || segment["quality_ok"] != true
            || !(3000..=6000).contains(&sample.duration_ms)
            || sample.wav.len() > 192_044
        {
            continue;
        }
        let id = if segments.len() == 1 {
            format!("{turn}:whole")
        } else {
            format!("{turn}:s{}", sample.index)
        };
        if db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM voice_clips WHERE segment_id=?1)",
                [&id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(db_error)?
        {
            continue;
        }
        let vector = normalize(&sample.embedding)?;
        let model = checked_model_id(&sample.model_id)?;
        let (manual, person): (bool, Option<String>) = db
            .query_row(
                "SELECT annotation_set,annotated_person_id FROM household_segments WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_error)?;
        let matched = if manual {
            match person.as_deref() {
                Some(p) => person_group(db, p)?,
                None => None,
            }
        } else {
            nearest_group(db, turn, label, &model, &vector, floor, margin)?
        };
        let group = match matched {
            Some(g) => g,
            None => new_group(db, person.as_deref())?,
        };
        db.execute("INSERT INTO voice_clips(segment_id,group_id,model_id,embedding,wav,duration_ms) VALUES(?1,?2,?3,?4,?5,?6)",
            params![id,group,model,encode(&vector),sample.wav,sample.duration_ms]).map_err(db_error)?;
        // Later observations of a named group are candidates, never human annotations.
        db.execute("UPDATE household_segments SET automatic_person_id=COALESCE(automatic_person_id,(SELECT person_id FROM voice_groups WHERE id=?2)) WHERE id=?1",params![id,group]).map_err(db_error)?;
    }
    db.execute(
        "DELETE FROM voice_clips WHERE segment_id IN(SELECT c.segment_id FROM voice_clips c
        JOIN household_segments s ON s.id=c.segment_id JOIN household_turns t ON t.id=s.turn_id
        ORDER BY t.occurred_at_ms DESC,c.rowid DESC LIMIT -1 OFFSET ?1)",
        [MAX_CLIPS],
    )
    .map_err(db_error)?;
    Ok(())
}

fn checked_group(db: &Connection, p: &Value) -> Result<i64, String> {
    let group = p["group_id"].as_i64().ok_or("voice_group_required")?;
    let current: Option<i64> = db
        .query_row(
            "SELECT revision FROM voice_groups WHERE id=?1",
            [group],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if current.is_none() || current != p["revision"].as_i64() {
        return Err("voice_group_changed".into());
    }
    Ok(group)
}

pub(super) fn source_ids(db: &Connection, method: &str, p: &Value) -> Result<Vec<String>, String> {
    let group = checked_group(db, p)?;
    let segment = if method == "household.voice.separate" {
        Some(p["segment_id"].as_str().ok_or("voice_clip_required")?)
    } else {
        None
    };
    let mut q=db.prepare("SELECT DISTINCT s.turn_id FROM voice_clips c JOIN household_segments s ON s.id=c.segment_id
        WHERE c.group_id=?1 AND (?2 IS NULL OR c.segment_id=?2)").map_err(db_error)?;
    let rows = q
        .query_map(params![group, segment], |r| r.get(0))
        .map_err(db_error)?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    if ids.is_empty() {
        return Err("voice_group_changed".into());
    }
    Ok(ids)
}

pub(super) fn individual_annotation(
    db: &Connection,
    segment: &str,
    person: Option<&str>,
    reset: bool,
) -> Result<(), String> {
    let old=db.query_row("SELECT g.id,g.person_id FROM voice_clips c JOIN voice_groups g ON g.id=c.group_id WHERE c.segment_id=?1",[segment],
        |r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?))).optional().map_err(db_error)?;
    let Some((group, owner)) = old else {
        return Ok(());
    };
    if owner.as_deref() != person || (!reset && person.is_none()) {
        let dest = if let Some(person) = person {
            person_group(db, person)?
        } else {
            None
        };
        let dest = match dest {
            Some(id) => id,
            None => new_group(db, person)?,
        };
        db.execute(
            "UPDATE voice_clips SET group_id=?1,group_annotation_revision=NULL WHERE segment_id=?2",
            params![dest, segment],
        )
        .map_err(db_error)?;
    } else {
        db.execute(
            "UPDATE voice_clips SET group_annotation_revision=NULL WHERE segment_id=?1",
            [segment],
        )
        .map_err(db_error)?;
        db.execute(
            "UPDATE voice_groups SET revision=revision+1 WHERE id=?1",
            [group],
        )
        .map_err(db_error)?;
    }
    Ok(())
}

impl Store {
    pub(crate) fn voice_collection_for(&self, turn: &str) -> Result<bool, String> {
        self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM household_turns t CROSS JOIN households h WHERE t.external_turn_id=?1
            AND t.voice_review_generation=h.voice_review_generation AND h.voice_review_enabled AND t.forgotten=0 AND t.source_expired=0 AND t.private_profile_id IS NULL)",[turn],|r|r.get(0)).map_err(db_error)
    }
    pub(super) fn voice_review_settings(&self, p: &Value) -> Result<Value, String> {
        let enabled = p["enabled"].as_bool().ok_or("voice_setting_required")?;
        let mut db = self.connection()?;
        let tx = db.transaction().map_err(db_error)?;
        tx.execute("UPDATE households SET voice_review_enabled=?1,voice_review_generation=voice_review_generation+1 WHERE voice_review_enabled!=?1",[enabled]).map_err(db_error)?;
        if !enabled {
            tx.execute("DELETE FROM voice_groups", [])
                .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok(json!({"enabled":enabled}))
    }
    pub(super) fn voice_album(&self) -> Result<Value, String> {
        self.purge_household_expired()?;
        let db = self.connection()?;
        let enabled: bool = db
            .query_row(
                "SELECT voice_review_enabled FROM households WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let mut q=db.prepare("SELECT g.id,g.revision,g.person_id,p.display_name,count(*),max(t.occurred_at_ms)
            FROM voice_groups g JOIN voice_clips c ON c.group_id=g.id JOIN household_segments s ON s.id=c.segment_id
            JOIN household_turns t ON t.id=s.turn_id LEFT JOIN profiles p ON p.id=g.person_id GROUP BY g.id ORDER BY g.id").map_err(db_error)?;
        let rows=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"revision":r.get::<_,i64>(1)?,"person_id":r.get::<_,Option<String>>(2)?,
            "name":r.get::<_,Option<String>>(3)?,"count":r.get::<_,usize>(4)?,"latest_ms":r.get::<_,i64>(5)?}))).map_err(db_error)?;
        Ok(
            json!({"enabled":enabled,"groups":rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?}),
        )
    }
    pub(super) fn voice_group_clips(&self, p: &Value) -> Result<Value, String> {
        self.purge_household_expired()?;
        let db = self.connection()?;
        let group = checked_group(&db, p)?;
        let mut q=db.prepare("SELECT c.segment_id,c.duration_ms,t.occurred_at_ms,s.start_sample,s.sample_rate,s.annotation_set
            FROM voice_clips c JOIN household_segments s ON s.id=c.segment_id JOIN household_turns t ON t.id=s.turn_id
            WHERE c.group_id=?1 ORDER BY row_number() OVER (PARTITION BY t.id ORDER BY c.duration_ms DESC,c.segment_id),t.occurred_at_ms DESC,s.start_sample,c.segment_id").map_err(db_error)?;
        let rows=q.query_map([group],|r|Ok(json!({"id":r.get::<_,String>(0)?,"duration_ms":r.get::<_,i64>(1)?,"at_ms":r.get::<_,i64>(2)?,
            "start_sample":r.get::<_,Option<i64>>(3)?,"sample_rate":r.get::<_,Option<i64>>(4)?,"annotated":r.get::<_,bool>(5)?}))).map_err(db_error)?;
        Ok(json!(rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error)?))
    }
    pub(super) fn voice_sample_audio(&self, p: &Value) -> Result<Value, String> {
        self.purge_household_expired()?;
        let db = self.connection()?;
        let group = checked_group(&db, p)?;
        let bytes: Vec<u8> = db
            .query_row(
                "SELECT wav FROM voice_clips WHERE group_id=?1 AND segment_id=?2",
                params![
                    group,
                    p["segment_id"].as_str().ok_or("voice_clip_required")?
                ],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or("voice_clip_unavailable")?;
        Ok(json!({"mime":"audio/wav","base64":STANDARD.encode(bytes)}))
    }
    pub(super) fn edit_voice_group(&self, method: &str, p: &Value) -> Result<Value, String> {
        let mut db = self.connection()?;
        let tx = db.transaction().map_err(db_error)?;
        let group = checked_group(&tx, p)?;
        let split = method == "household.voice.separate";
        let segment = if split {
            Some(p["segment_id"].as_str().ok_or("voice_clip_required")?)
        } else {
            None
        };
        let rows = {
            let mut q=tx.prepare("SELECT s.id,s.turn_id,t.episode_id,s.annotation_set,s.annotated_person_id,s.annotation_revision,c.group_annotation_revision
                FROM voice_clips c JOIN household_segments s ON s.id=c.segment_id JOIN household_turns t ON t.id=s.turn_id
                WHERE c.group_id=?1 AND (?2 IS NULL OR c.segment_id=?2)").map_err(db_error)?;
            let it = q
                .query_map(params![group, segment], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, bool>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                    ))
                })
                .map_err(db_error)?;
            it.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?
        };
        if rows.is_empty() {
            return Err("voice_group_changed".into());
        }
        let person = if split {
            None
        } else if let Some(id) = p["person_id"].as_str() {
            super::profile_merge::active_profile(&tx, id)?;
            Some(id.to_owned())
        } else {
            let name = super::store::clean_text(
                p["name"].as_str().ok_or("voice_name_required")?,
                80,
                false,
                "display_name",
            )?;
            // The picker uses an ID for existing people; typed new names must be unambiguous.
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM profiles WHERE display_name=?1)",
                    [&name],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if exists {
                return Err("voice_choose_existing_person".into());
            }
            let profile = insert_profile(
                &tx,
                NewProfile {
                    display_name: name,
                    is_child: false,
                },
                self.config.max_profiles,
            )?;
            bump_revision(&tx)?;
            Some(profile.id)
        };
        let target = if let Some(id) = &person {
            person_group(&tx, id)?.unwrap_or(group)
        } else {
            new_group(&tx, None)?
        };
        if !split {
            tx.execute(
                "UPDATE voice_groups SET person_id=?1,revision=revision+1 WHERE id=?2",
                params![person, target],
            )
            .map_err(db_error)?;
        }
        for (clip, turn, episode, set, previous, rev, group_rev) in rows {
            if split {
                // The user explicitly removed this particular voice. Withdraw its
                // old source attribution as well as moving the review excerpt.
                annotate_segment(&tx, &clip, None, true, &turn, &episode)?;
                tx.execute(
                    "UPDATE household_segments SET automatic_person_id=NULL WHERE id=?1",
                    [&clip],
                )
                .map_err(db_error)?;
                tx.execute(
                    "UPDATE voice_clips SET group_annotation_revision=NULL WHERE segment_id=?1",
                    [&clip],
                )
                .map_err(db_error)?;
            } else if set && group_rev != Some(rev) {
                // A separate human decision on one recording takes precedence.
                if previous != person {
                    individual_annotation(&tx, &clip, previous.as_deref(), false)?;
                    continue;
                }
            } else {
                annotate_segment(&tx, &clip, person.as_deref(), false, &turn, &episode)?;
                tx.execute("UPDATE voice_clips SET group_annotation_revision=(SELECT annotation_revision FROM household_segments WHERE id=?1) WHERE segment_id=?1",[&clip]).map_err(db_error)?;
            }
            if target != group {
                tx.execute(
                    "UPDATE voice_clips SET group_id=?1 WHERE segment_id=?2",
                    params![target, clip],
                )
                .map_err(db_error)?;
            }
        }
        tx.commit().map_err(db_error)?;
        Ok(json!({"group_id":target,"person_id":person}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{JournalInput, MemoryConfig};
    use std::path::PathBuf;

    struct Home(PathBuf);
    impl Home {
        fn fresh() -> Self {
            Self(std::env::temp_dir().join(format!("voice-album-{}", uuid::Uuid::new_v4())))
        }
        fn open(&self) -> std::sync::Arc<Store> {
            Store::open(self.0.clone(), MemoryConfig::default()).unwrap()
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn input(store: &Store, id: &str, at: i64, private: Option<String>) -> u64 {
        store
            .journal_input(&JournalInput {
                turn_id: id.into(),
                activation_id: id.into(),
                utterance_id: None,
                text: String::new(),
                occurred_at_ms: at,
                input_kind: "audio".into(),
                speaker_person_id: None,
                private_profile_id: private,
                allow_cloud: false,
                metadata: json!({}),
            })
            .unwrap()
            .source_revision
    }
    fn observation(store: &Store, id: &str, revision: u64, spans: &[(&str, usize, bool)]) {
        let mut segments = Vec::new();
        let mut samples = Vec::new();
        for (index, (label, axis, overlap)) in spans.iter().enumerate() {
            let mut vector = vec![0f32; 192];
            vector[*axis] = 1.;
            let start = index * 64_000;
            segments.push(json!({"speaker_label":label,"start_sample":start,"end_sample":start+64_000,"sample_rate":16000,"single_speaker_verified":!overlap,"overlap":overlap,"quality_ok":true}));
            let mut wav = vec![0u8; 128044];
            wav[..4].copy_from_slice(b"RIFF");
            samples.push(VoiceSample {
                index,
                model_id: "test:ecapa".into(),
                embedding: vector,
                wav,
                duration_ms: 4000,
            });
        }
        store
            .journal_identity_samples(id, revision, json!({"segments":segments}), &samples)
            .unwrap();
    }
    fn groups(store: &Store) -> Vec<Value> {
        store.voice_album().unwrap()["groups"]
            .as_array()
            .unwrap()
            .clone()
    }
    fn group_ref(g: &Value) -> Value {
        json!({"group_id":g["id"],"revision":g["revision"]})
    }

    #[test]
    fn discovers_without_enrollment_and_names_past_and_future_turns() {
        let home = Home::fresh();
        let store = home.open();
        let now = now_ms();
        store
            .voice_review_settings(&json!({"enabled":true}))
            .unwrap();
        for (id, axis) in [("a", 0), ("b", 1), ("c", 0)] {
            let rev = input(&store, id, now, None);
            observation(&store, id, rev, &[("SPEAKER_00", axis, false)]);
        }
        let g = groups(&store);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0]["count"], 2);
        assert_eq!(g[1]["count"], 1);
        let payload = group_ref(&g[0]);
        let clips = store.voice_group_clips(&payload).unwrap();
        let mut name = payload.clone();
        name["name"] = json!("甲");
        let named = store
            .edit_voice_group("household.voice.name", &name)
            .unwrap();
        let person = named["person_id"].as_str().unwrap();
        assert!(store
            .edit_voice_group("household.voice.name", &name)
            .is_err());
        for id in ["a", "c"] {
            let src = store
                .household_rpc("household.source", &json!({"conversation_turn_id":id}))
                .unwrap()
                .unwrap();
            assert_eq!(src["segments"][0]["annotated_person_id"], person);
            assert_eq!(src["segments"][0]["annotation_set"], true);
        }
        let rev = input(&store, "d", now, None);
        observation(&store, "d", rev, &[("ARBITRARY_LOCAL_LABEL", 0, false)]);
        let src = store
            .household_rpc("household.source", &json!({"conversation_turn_id":"d"}))
            .unwrap()
            .unwrap();
        assert_eq!(src["segments"][0]["automatic_person_id"], person);
        assert_eq!(src["segments"][0]["annotation_set"], false);
        // §12.1: a verified single-speaker match may be said aloud; an annotated
        // turn keeps its name; a turn with overlapping voices is never nameable.
        let speaker = store.household_turn_speaker("d").unwrap().unwrap();
        assert_eq!((speaker.person_id.as_str(), speaker.display_name.as_str(), speaker.nameable), (person, "甲", true));
        assert_eq!(store.household_turn_speaker("a").unwrap().unwrap().display_name, "甲");
        let rev = input(&store, "e", now, None);
        observation(&store, "e", rev, &[("SPEAKER_00", 0, true)]);
        assert!(store.household_turn_speaker("e").unwrap().is_none_or(|s| !s.nameable));
        assert!(store.household_turn_speaker("b").unwrap().is_none());
        store.journal_speaker_correction("d", person).unwrap();
        drop(store);
        let store = home.open();
        let g = groups(&store);
        assert_eq!(g[0]["name"], "甲");
        assert_eq!(g[0]["count"], 3);
        let mut split = group_ref(&g[0]);
        split["segment_id"] = clips[0]["id"].clone();
        store
            .edit_voice_group("household.voice.separate", &split)
            .unwrap();
        let g = groups(&store);
        assert_eq!(g.len(), 3);
        assert_eq!(g[0]["count"], 2);
        let mut merge = group_ref(&g[2]);
        merge["person_id"] = json!(person);
        store
            .edit_voice_group("household.voice.name", &merge)
            .unwrap();
        assert_eq!(groups(&store).len(), 2);
        assert_eq!(groups(&store)[0]["count"], 3);
        store.delete_profile(person).unwrap();
        assert_eq!(groups(&store).len(), 1);
    }

    #[test]
    fn excludes_private_mixed_expired_and_before_opt_in_turns() {
        let home = Home::fresh();
        let store = home.open();
        let now = now_ms();
        let early = input(&store, "before", now, None);
        store
            .voice_review_settings(&json!({"enabled":true}))
            .unwrap();
        observation(&store, "before", early, &[("x", 0, false)]);
        let person = store
            .create_profile(NewProfile {
                display_name: "私密".into(),
                is_child: false,
            })
            .unwrap();
        let rev = input(&store, "private", now, Some(person.id));
        observation(&store, "private", rev, &[("x", 0, false)]);
        let old = input(&store, "pending", now, None);
        store
            .voice_review_settings(&json!({"enabled":false}))
            .unwrap();
        store
            .voice_review_settings(&json!({"enabled":true}))
            .unwrap();
        observation(&store, "pending", old, &[("x", 0, false)]);
        assert!(groups(&store).is_empty());
        let rev = input(&store, "multi", now, None);
        observation(
            &store,
            "multi",
            rev,
            &[("one", 0, false), ("two", 0, false), ("three", 2, true)],
        );
        let g = groups(&store);
        assert_eq!(g.len(), 2);
        assert!(g.iter().all(|g| g["count"] == 1));
        let mut listen = group_ref(&g[0]);
        listen["segment_id"] = store.voice_group_clips(&listen).unwrap()[0]["id"].clone();
        assert!(!store.voice_sample_audio(&listen).unwrap()["base64"]
            .as_str()
            .unwrap()
            .is_empty());
        let mut other = group_ref(&g[1]);
        other["segment_id"] = listen["segment_id"].clone();
        assert!(store.voice_sample_audio(&other).is_err());
        // Excerpt expiry is independent of the household's configurable source TTL.
        store
            .update_household_settings(&json!({"history_enabled":true,"retention_secs":90*86400}))
            .unwrap();
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE household_turns SET occurred_at_ms=?1 WHERE external_turn_id='multi'",
                [now - MAX_AGE_MS - 1000],
            )
            .unwrap();
        assert!(groups(&store).is_empty());
        assert!(store.voice_sample_audio(&listen).is_err());
        let revision = input(&store, "forgotten", now, None);
        observation(&store, "forgotten", revision, &[("one", 0, false)]);
        store
            .household_rpc(
                "household.forget",
                &json!({"conversation_turn_id":"forgotten"}),
            )
            .unwrap();
        assert!(groups(&store).is_empty());
    }
}
