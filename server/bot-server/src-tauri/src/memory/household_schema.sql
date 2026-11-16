-- Additive migration. Legacy profile-owned transcripts and private notes stay
-- in their original tables; importing metadata never makes private text shared.
CREATE TABLE households (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    id TEXT NOT NULL UNIQUE,
    timezone TEXT NOT NULL,
    timezone_source TEXT NOT NULL,
    history_enabled INTEGER NOT NULL CHECK(history_enabled IN(0,1)),
    retention_secs INTEGER NOT NULL,
    allow_cloud INTEGER NOT NULL CHECK(allow_cloud IN(0,1)),
    policy_revision INTEGER NOT NULL DEFAULT 1,
    graph_engine_signature TEXT,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE profile_aliases (
    source_profile_id TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    target_profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    merged_at_ms INTEGER NOT NULL,
    CHECK(source_profile_id!=target_profile_id)
);
CREATE TABLE profile_merge_events (
    id TEXT PRIMARY KEY,
    source_profile_id TEXT NOT NULL,
    target_profile_id TEXT NOT NULL,
    merged_at_ms INTEGER NOT NULL,
    archived_enrollments INTEGER NOT NULL
);
-- Archived templates keep their original ownership, samples, model ID and
-- consent provenance, but never participate in current matching or training.
CREATE VIEW active_enrollments AS
    SELECT e.* FROM enrollments e
    WHERE NOT EXISTS(SELECT 1 FROM profile_aliases a WHERE a.source_profile_id=e.profile_id);
CREATE TABLE household_days (
    id TEXT PRIMARY KEY,
    household_id TEXT NOT NULL REFERENCES households(id),
    local_date TEXT NOT NULL,
    timezone TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(household_id,local_date)
);
CREATE TABLE household_episodes (
    id TEXT PRIMARY KEY,
    daily_id TEXT NOT NULL REFERENCES household_days(id),
    activation_id TEXT NOT NULL,
    opened_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER,
    revision INTEGER NOT NULL DEFAULT 1,
    processed_version TEXT,
    indexed_version TEXT
);
CREATE INDEX household_episode_day ON household_episodes(daily_id,opened_at_ms);
CREATE TABLE household_turns (
    id TEXT PRIMARY KEY,
    external_turn_id TEXT NOT NULL UNIQUE,
    episode_id TEXT NOT NULL REFERENCES household_episodes(id),
    utterance_id TEXT NOT NULL,
    input_kind TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    source_revision INTEGER NOT NULL DEFAULT 1,
    input_text TEXT NOT NULL DEFAULT '',
    assistant_text TEXT NOT NULL DEFAULT '',
    private_profile_id TEXT,
    allow_cloud INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'received',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    source_expired INTEGER NOT NULL DEFAULT 0,
    forgotten INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX household_turn_order ON household_turns(episode_id,occurred_at_ms,recorded_at_ms);
CREATE INDEX household_turn_retention ON household_turns(source_expired,forgotten,occurred_at_ms);
CREATE TABLE household_source_revisions (
    turn_id TEXT NOT NULL REFERENCES household_turns(id),
    revision INTEGER NOT NULL,
    input_text TEXT NOT NULL,
    reason TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    PRIMARY KEY(turn_id,revision)
);
CREATE TABLE household_segments (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL REFERENCES household_turns(id),
    start_sample INTEGER,
    end_sample INTEGER,
    sample_rate INTEGER,
    speaker_label TEXT,
    candidate_json TEXT NOT NULL DEFAULT '{}',
    automatic_person_id TEXT,
    annotated_person_id TEXT,
    annotation_set INTEGER NOT NULL DEFAULT 0,
    annotation_revision INTEGER NOT NULL DEFAULT 0,
    overlap INTEGER NOT NULL DEFAULT 0,
    quality TEXT NOT NULL DEFAULT 'unknown',
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX household_segments_turn ON household_segments(turn_id);
CREATE TABLE household_identity_observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    turn_id TEXT NOT NULL REFERENCES household_turns(id),
    source_revision INTEGER NOT NULL,
    candidate_json TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);
CREATE TABLE household_annotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    segment_id TEXT NOT NULL REFERENCES household_segments(id),
    person_id TEXT,
    revision INTEGER NOT NULL,
    operation TEXT NOT NULL DEFAULT 'set',
    at_ms INTEGER NOT NULL
);
CREATE TABLE household_receipts (
    event_key TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL REFERENCES household_turns(id),
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);
CREATE TABLE household_jobs (
    id TEXT PRIMARY KEY,
    episode_id TEXT NOT NULL REFERENCES household_episodes(id),
    kind TEXT NOT NULL CHECK(kind IN('organize','graph_ingest','graph_delete')),
    input_version TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    allow_cloud INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'queued',
    lease TEXT,
    lease_until_ms INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at_ms INTEGER NOT NULL,
    last_error TEXT,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(episode_id,kind,input_version)
);
CREATE INDEX household_job_ready ON household_jobs(state,available_at_ms,created_at_ms);
CREATE TABLE household_derived (
    id TEXT PRIMARY KEY,
    episode_id TEXT NOT NULL REFERENCES household_episodes(id),
    input_version TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN('event','topic','learning','fact')),
    content TEXT NOT NULL,
    subject_ids_json TEXT NOT NULL,
    topic_id TEXT,
    evidence TEXT,
    learning_json TEXT NOT NULL DEFAULT '{}',
    assertion_kind TEXT NOT NULL,
    sources_json TEXT NOT NULL,
    source_ref TEXT NOT NULL DEFAULT ('household-memory-ref:'||lower(hex(randomblob(16)))),
    confirmed INTEGER NOT NULL DEFAULT 0,
    allow_cloud INTEGER NOT NULL DEFAULT 0,
    private_profile_id TEXT,
    valid INTEGER NOT NULL DEFAULT 1,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX household_derived_kind ON household_derived(valid,kind,created_at_ms);
CREATE TABLE household_derived_sources (
    derived_id TEXT NOT NULL REFERENCES household_derived(id) ON DELETE CASCADE,
    turn_id TEXT NOT NULL REFERENCES household_turns(id),
    source_revision INTEGER NOT NULL,
    PRIMARY KEY(derived_id,turn_id)
);
CREATE TABLE household_tombstones (
    scope TEXT NOT NULL,
    target_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    at_ms INTEGER NOT NULL,
    PRIMARY KEY(scope,target_id)
);
CREATE TABLE household_legacy_links (
    legacy_conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    household_id TEXT NOT NULL REFERENCES households(id),
    private_profile_id TEXT,
    imported_at_ms INTEGER NOT NULL
);
CREATE TABLE household_model_conversations (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    daily_id TEXT NOT NULL REFERENCES household_days(id),
    private_profile_id TEXT
);
CREATE TABLE legacy_memory_evidence (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL,
    job_id TEXT,
    quote TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);
CREATE TABLE household_graph_projection (
    episode_id TEXT PRIMARY KEY REFERENCES household_episodes(id),
    graph_uuid TEXT NOT NULL,
    input_version TEXT NOT NULL,
    state TEXT NOT NULL,
    engine_signature TEXT NOT NULL DEFAULT '',
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE household_graph_rebuild (
    episode_id TEXT PRIMARY KEY REFERENCES household_episodes(id),
    requested_at_ms INTEGER NOT NULL
);
-- Cross-store media/task cleanup is acknowledged only after the task store and
-- artifact store have completed it. A crash cannot lose a TTL/forget request.
CREATE TABLE household_source_cleanup (
    -- Opaque dependency key: a canonical turn ID, a versioned source-ref, or a
    -- legacy-memory-ref. It deliberately has no foreign key to retained text.
    turn_id TEXT PRIMARY KEY,
    source_revision INTEGER NOT NULL,
    queued_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER
);
CREATE TABLE household_source_ref_versions (
    turn_id TEXT NOT NULL REFERENCES household_turns(id),
    revision INTEGER NOT NULL,
    source_ref TEXT NOT NULL UNIQUE,
    PRIMARY KEY(turn_id,revision)
);
CREATE TRIGGER household_source_ref_insert AFTER INSERT ON household_turns BEGIN
    INSERT INTO household_source_ref_versions(turn_id,revision,source_ref)
    VALUES(NEW.id,NEW.source_revision,'source-ref:'||NEW.id||':v'||NEW.source_revision);
END;
CREATE TRIGGER household_source_ref_update AFTER UPDATE OF source_revision ON household_turns
WHEN OLD.source_revision!=NEW.source_revision BEGIN
    INSERT OR IGNORE INTO household_source_ref_versions(turn_id,revision,source_ref)
    VALUES(NEW.id,NEW.source_revision,'source-ref:'||NEW.id||':v'||NEW.source_revision);
END;
-- Epoch keys change on semantic/permission edits even if a user restores the
-- previous text or recreates the same memory ID. Task fences are permanent, so
-- a content hash alone would incorrectly ban a later explicitly saved note.
CREATE TABLE legacy_memory_source_refs (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    source_ref TEXT NOT NULL UNIQUE
);
INSERT INTO legacy_memory_source_refs(memory_id,source_ref)
    SELECT id,'legacy-memory-ref:'||lower(hex(randomblob(16))) FROM memories;
CREATE TRIGGER legacy_memory_source_insert AFTER INSERT ON memories BEGIN
    INSERT INTO legacy_memory_source_refs(memory_id,source_ref)
    VALUES(NEW.id,'legacy-memory-ref:'||lower(hex(randomblob(16))));
END;
CREATE TRIGGER legacy_memory_source_retire BEFORE UPDATE ON memories
WHEN OLD.scope IS NOT NEW.scope OR OLD.profile_id IS NOT NEW.profile_id
 OR OLD.content IS NOT NEW.content OR OLD.category IS NOT NEW.category
 OR OLD.source IS NOT NEW.source OR OLD.source_ref IS NOT NEW.source_ref
 OR OLD.confirmed IS NOT NEW.confirmed OR OLD.allow_cloud IS NOT NEW.allow_cloud
 OR OLD.expires_at_ms IS NOT NEW.expires_at_ms BEGIN
    INSERT INTO household_source_cleanup(turn_id,source_revision,queued_at_ms)
    SELECT source_ref,0,CAST(strftime('%s','now') AS INTEGER)*1000
    FROM legacy_memory_source_refs WHERE memory_id=OLD.id
    ON CONFLICT(turn_id) DO UPDATE SET queued_at_ms=excluded.queued_at_ms,completed_at_ms=NULL;
END;
CREATE TRIGGER legacy_memory_source_replace AFTER UPDATE ON memories
WHEN OLD.scope IS NOT NEW.scope OR OLD.profile_id IS NOT NEW.profile_id
 OR OLD.content IS NOT NEW.content OR OLD.category IS NOT NEW.category
 OR OLD.source IS NOT NEW.source OR OLD.source_ref IS NOT NEW.source_ref
 OR OLD.confirmed IS NOT NEW.confirmed OR OLD.allow_cloud IS NOT NEW.allow_cloud
 OR OLD.expires_at_ms IS NOT NEW.expires_at_ms BEGIN
    UPDATE legacy_memory_source_refs SET source_ref='legacy-memory-ref:'||lower(hex(randomblob(16)))
    WHERE memory_id=NEW.id;
END;
CREATE TRIGGER legacy_memory_source_delete BEFORE DELETE ON memories BEGIN
    INSERT INTO household_source_cleanup(turn_id,source_revision,queued_at_ms)
    SELECT source_ref,0,CAST(strftime('%s','now') AS INTEGER)*1000
    FROM legacy_memory_source_refs WHERE memory_id=OLD.id
    ON CONFLICT(turn_id) DO UPDATE SET queued_at_ms=excluded.queued_at_ms,completed_at_ms=NULL;
END;
-- Only retained, independently confirmed facts expose this dependency after
-- their original transcript expires. Re-extraction cannot reuse a retired key.
CREATE TRIGGER household_memory_source_delete BEFORE DELETE ON household_derived BEGIN
    INSERT INTO household_source_cleanup(turn_id,source_revision,queued_at_ms)
    VALUES(OLD.source_ref,0,CAST(strftime('%s','now') AS INTEGER)*1000)
    ON CONFLICT(turn_id) DO UPDATE SET queued_at_ms=excluded.queued_at_ms,completed_at_ms=NULL;
END;
CREATE TRIGGER household_memory_source_retire BEFORE UPDATE ON household_derived
WHEN OLD.content IS NOT NEW.content OR OLD.sources_json IS NOT NEW.sources_json
 OR OLD.subject_ids_json IS NOT NEW.subject_ids_json OR OLD.private_profile_id IS NOT NEW.private_profile_id
 OR OLD.allow_cloud IS NOT NEW.allow_cloud OR OLD.learning_json IS NOT NEW.learning_json
 OR OLD.evidence IS NOT NEW.evidence OR OLD.assertion_kind IS NOT NEW.assertion_kind
 OR (OLD.valid=1 AND NEW.valid=0) BEGIN
    INSERT INTO household_source_cleanup(turn_id,source_revision,queued_at_ms)
    VALUES(OLD.source_ref,0,CAST(strftime('%s','now') AS INTEGER)*1000)
    ON CONFLICT(turn_id) DO UPDATE SET queued_at_ms=excluded.queued_at_ms,completed_at_ms=NULL;
END;
CREATE TRIGGER household_memory_source_replace AFTER UPDATE ON household_derived
WHEN OLD.content IS NOT NEW.content OR OLD.sources_json IS NOT NEW.sources_json
 OR OLD.subject_ids_json IS NOT NEW.subject_ids_json OR OLD.private_profile_id IS NOT NEW.private_profile_id
 OR OLD.allow_cloud IS NOT NEW.allow_cloud OR OLD.learning_json IS NOT NEW.learning_json
 OR OLD.evidence IS NOT NEW.evidence OR OLD.assertion_kind IS NOT NEW.assertion_kind
 OR (OLD.valid=1 AND NEW.valid=0) BEGIN
    UPDATE household_derived SET source_ref='household-memory-ref:'||lower(hex(randomblob(16)))
    WHERE id=NEW.id;
END;
PRAGMA user_version = 5;
