-- Additive tables only (CREATE ... IF NOT EXISTS, no user_version change): older
-- builds ignore them, so rolling back the App keeps the database readable.
CREATE TABLE IF NOT EXISTS household_derived_dates (
    derived_id TEXT PRIMARY KEY REFERENCES household_derived(id) ON DELETE CASCADE,
    on_date TEXT,   -- YYYY-MM-DD for a one-off date
    yearly TEXT,    -- MM-DD for a date that recurs every year
    CHECK ((on_date IS NULL) <> (yearly IS NULL))
);
-- Guardian-entered details; never inferred from voice or conversation.
CREATE TABLE IF NOT EXISTS profile_details (
    profile_id TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    age_band TEXT NOT NULL CHECK (age_band IN ('toddler','child','tween','teen','adult','senior')),
    updated_at_ms INTEGER NOT NULL
);
-- Unprompted lines (UX review §12.2). Settings are one JSON row read with serde
-- defaults, so adding a field never breaks an older or newer App.
CREATE TABLE IF NOT EXISTS proactive_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    settings_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
-- Explicitly scheduled reminders; undelivered offers can be shown and retried once.
CREATE TABLE IF NOT EXISTS proactive_reminders (
    id TEXT PRIMARY KEY,
    profile_id TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    due_at_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','spoken','shown','missed','cancelled')),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL
);
-- Every unprompted line offered, with its trigger and what became of it.
CREATE TABLE IF NOT EXISTS proactive_journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    offer_id INTEGER NOT NULL,
    at_ms INTEGER NOT NULL,
    local_date TEXT NOT NULL,
    moment TEXT NOT NULL,
    trigger_source TEXT NOT NULL,
    line TEXT NOT NULL,
    person_id TEXT,
    device_id TEXT NOT NULL,
    status TEXT NOT NULL,
    reason TEXT
);
CREATE INDEX IF NOT EXISTS proactive_journal_day ON proactive_journal(local_date, at_ms);

-- Only names checked on a real write get a policy receipt. Existing names are
-- left intact and must be revalidated before being used as a spoken identity.
CREATE TABLE IF NOT EXISTS profile_name_validation (
    profile_id TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    policy_version TEXT NOT NULL,
    validated_at_ms INTEGER NOT NULL
);

-- The operation coordinator stores only a digest and a reference to this
-- domain transaction. No original transcript or audio is retained here.
CREATE TABLE IF NOT EXISTS assistant_domain_receipts (
    operation_id TEXT PRIMARY KEY,
    request_hash TEXT NOT NULL,
    source_turn TEXT NOT NULL REFERENCES household_turns(id) ON DELETE CASCADE,
    source_revision INTEGER NOT NULL,
    reminder_id TEXT REFERENCES proactive_reminders(id) ON DELETE CASCADE,
    result_json TEXT NOT NULL,
    committed_at_ms INTEGER NOT NULL
);
-- Explicit forgetting is distinguished from archive expiry by the domain service.
DROP TRIGGER IF EXISTS assistant_reminder_source_forgotten;

CREATE TRIGGER IF NOT EXISTS assistant_reminder_source_removed
BEFORE DELETE ON household_turns
BEGIN
    UPDATE proactive_reminders SET state='cancelled' WHERE state='pending' AND id IN
        (SELECT reminder_id FROM assistant_domain_receipts WHERE source_turn=OLD.id);
END;

CREATE TRIGGER IF NOT EXISTS assistant_reminder_source_corrected
AFTER UPDATE OF input_text ON household_turns
WHEN NEW.input_text!=OLD.input_text AND OLD.input_text!='' AND NEW.source_expired=0
BEGIN
    UPDATE proactive_reminders SET state='cancelled' WHERE state='pending' AND id IN
        (SELECT reminder_id FROM assistant_domain_receipts WHERE source_turn=NEW.id);
    DELETE FROM assistant_domain_receipts WHERE source_turn=NEW.id;
END;
