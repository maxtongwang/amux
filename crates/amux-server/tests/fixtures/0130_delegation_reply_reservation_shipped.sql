-- PR #18 Opus delta review of edc942b2: migration reversal. Keep the shipped
-- 0129 immutable so databases already stamped 129 receive these additions.
-- One owner reply reserves one request across actors, modes and statuses.
-- ADDCOL: _amux_delegation_grants cite_spent_at REAL
-- ADDCOL: _amux_delegation_grants target_incarnation TEXT
-- ADDCOL: cmd_history cite_worker_ids TEXT
CREATE UNIQUE INDEX IF NOT EXISTS idx_delegation_cite_reply
    ON _amux_delegation_grants(evidence_row)
    WHERE evidence_row IS NOT NULL;
