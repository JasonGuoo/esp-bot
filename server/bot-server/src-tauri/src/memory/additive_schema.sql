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
