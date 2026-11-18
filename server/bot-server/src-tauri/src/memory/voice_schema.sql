-- Opt-in excerpts from explicitly activated shared audio only. The capture
-- generation prevents delayed inference from reviving a disabled collection.
ALTER TABLE households ADD COLUMN voice_review_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE households ADD COLUMN voice_review_generation INTEGER NOT NULL DEFAULT 1;
ALTER TABLE household_turns ADD COLUMN voice_review_generation INTEGER;
CREATE TABLE voice_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    person_id TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE voice_clips (
    segment_id TEXT PRIMARY KEY REFERENCES household_segments(id) ON DELETE CASCADE,
    group_id INTEGER NOT NULL REFERENCES voice_groups(id) ON DELETE CASCADE,
    model_id TEXT NOT NULL,
    embedding BLOB NOT NULL,
    wav BLOB NOT NULL,
    duration_ms INTEGER NOT NULL,
    group_annotation_revision INTEGER
);
CREATE INDEX voice_clip_group ON voice_clips(group_id);
CREATE TRIGGER voice_clip_added AFTER INSERT ON voice_clips BEGIN
    UPDATE voice_groups SET revision=revision+1 WHERE id=NEW.group_id;
END;
CREATE TRIGGER voice_clip_removed AFTER DELETE ON voice_clips BEGIN
    UPDATE voice_groups SET revision=revision+1 WHERE id=OLD.group_id;
    DELETE FROM voice_groups WHERE id=OLD.group_id AND NOT EXISTS(SELECT 1 FROM voice_clips WHERE group_id=OLD.group_id);
END;
CREATE TRIGGER voice_clip_moved AFTER UPDATE OF group_id ON voice_clips WHEN OLD.group_id!=NEW.group_id BEGIN
    UPDATE voice_groups SET revision=revision+1 WHERE id IN(OLD.group_id,NEW.group_id);
    DELETE FROM voice_groups WHERE id=OLD.group_id AND NOT EXISTS(SELECT 1 FROM voice_clips WHERE group_id=OLD.group_id);
END;
PRAGMA user_version=6;
