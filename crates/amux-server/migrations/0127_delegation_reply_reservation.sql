-- PR #18 Opus delta review of edc942b2: migration reversal. Keep the shipped
-- 0126 immutable so databases already stamped 126 receive these additions.
-- One owner reply reserves one request across actors, modes and statuses.
-- ADDCOL: _amux_delegation_grants cite_spent_at REAL
-- ADDCOL: _amux_delegation_grants target_incarnation TEXT
-- ADDCOL: cmd_history cite_worker_ids TEXT
-- ADDCOL: _amux_delegation_grants legacy_duplicate INTEGER NOT NULL DEFAULT 0
-- Preserve every historical grant and its evidence. Earlier builds could reuse
-- a reply; only its earliest grant reserves it in the unique index below.
UPDATE _amux_delegation_grants
SET legacy_duplicate = 1
WHERE rowid IN (
    SELECT grant_rowid FROM (
        SELECT rowid AS grant_rowid,
            ROW_NUMBER() OVER (PARTITION BY evidence_row ORDER BY created_at, rowid) AS reservation_order
        FROM _amux_delegation_grants
        WHERE evidence_row IS NOT NULL
    ) WHERE reservation_order > 1
);
UPDATE _amux_delegation_grants
SET cite_spent_at = COALESCE(cite_spent_at, created_at)
WHERE evidence_row IS NOT NULL AND legacy_duplicate = 0;
CREATE UNIQUE INDEX IF NOT EXISTS idx_delegation_cite_reply
    ON _amux_delegation_grants(evidence_row)
    WHERE evidence_row IS NOT NULL AND legacy_duplicate = 0;
