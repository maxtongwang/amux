-- Preserve historical grants while reserving each used reply once. Runs even
-- when the DB already completed the shipped 0127 with its full reply index.
-- ADDCOL: _amux_delegation_grants legacy_duplicate INTEGER NOT NULL DEFAULT 0
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
WHERE evidence_row IS NOT NULL;
DROP INDEX IF EXISTS idx_delegation_cite_reply;
CREATE UNIQUE INDEX IF NOT EXISTS idx_delegation_cite_reply
    ON _amux_delegation_grants(evidence_row)
    WHERE evidence_row IS NOT NULL AND legacy_duplicate = 0;
