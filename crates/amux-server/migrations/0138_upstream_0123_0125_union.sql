-- 0138: idempotent union of upstream's 0123-0125 for databases that recorded
-- the fork's own migrations at those versions.
--
-- ADDITIVE ONLY (shared live DB).
--
-- WHY (2026-10-26 upstream merge): the fork and upstream (mixpeek/amux) each
-- numbered three different migrations 0123-0125. The fork's moved to
-- 0135-0137. A database that already recorded the fork's names at 123-125
-- skips upstream's three (the runner dedupes on version alone), so this file
-- applies their schema again in a form that is safe whether or not it ran.
-- On a fresh database it is a no-op.

-- 0123_file_opens
CREATE TABLE IF NOT EXISTS file_opens (
    path TEXT PRIMARY KEY,
    opened_at REAL NOT NULL
);

-- 0124_pinned_notes_groups (a plain ALTER there; ADDCOL here)
-- ADDCOL: pinned_notes group_id TEXT
CREATE INDEX IF NOT EXISTS idx_pinned_notes_group ON pinned_notes(group_id) WHERE group_id IS NOT NULL;

-- 0125_schedule_lifecycle
-- ADDCOL: schedules not_before TEXT
-- ADDCOL: schedules until TEXT
-- ADDCOL: schedules max_runs INTEGER
-- ADDCOL: schedules cron_runs INTEGER NOT NULL DEFAULT 0
-- ADDCOL: schedules locked_by TEXT
-- ADDCOL: schedules lock_reason TEXT
-- ADDCOL: schedules locked_at INTEGER
-- ADDCOL: schedules disabled_reason TEXT
