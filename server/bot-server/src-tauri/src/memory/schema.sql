CREATE TABLE profiles (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    is_child INTEGER NOT NULL CHECK (is_child IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE memories (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL CHECK (scope IN ('household', 'profile')),
    profile_id TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    category TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('guardian_note', 'user_statement', 'assistant_suggestion')),
    source_ref TEXT,
    confirmed INTEGER NOT NULL CHECK (confirmed IN (0, 1)),
    allow_cloud INTEGER NOT NULL CHECK (allow_cloud IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER,
    CHECK ((scope = 'household' AND profile_id IS NULL)
        OR (scope = 'profile' AND profile_id IS NOT NULL))
);
CREATE INDEX memories_scope ON memories(scope, profile_id, confirmed);
CREATE INDEX memories_expiry ON memories(expires_at_ms);

CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    profile_id TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX conversation_profile ON conversations(profile_id);
CREATE TABLE turns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);
CREATE INDEX turns_conversation ON turns(conversation_id, id);
CREATE INDEX turns_expiry ON turns(at_ms);

CREATE TABLE enrollments (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('face', 'speaker')),
    model_id TEXT NOT NULL,
    dimension INTEGER NOT NULL CHECK (dimension BETWEEN 32 AND 4096),
    sample_count INTEGER NOT NULL CHECK (sample_count BETWEEN 3 AND 32),
    centroid BLOB NOT NULL,
    consent_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE (profile_id, kind)
);
CREATE INDEX enrollment_model ON enrollments(kind, model_id);
CREATE TABLE embedding_samples (
    enrollment_id TEXT NOT NULL REFERENCES enrollments(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    embedding BLOB NOT NULL,
    PRIMARY KEY (enrollment_id, ordinal)
);

CREATE TABLE data_revision (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL
);
INSERT INTO data_revision(singleton, revision) VALUES (1, 0);
PRAGMA user_version = 1;
