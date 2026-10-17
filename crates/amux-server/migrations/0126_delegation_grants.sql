-- Structured grants (maxtongwang/ai-config#160): a widening change mrpopo
-- asks for, stored exactly, applied once on Max's approval. Times are epoch
-- seconds (now_f64). Status: pending, applying, applied, failed, denied,
-- expired. Transitions are also appended to the delegated-change log.
CREATE TABLE IF NOT EXISTS _amux_delegation_grants (
    code         TEXT PRIMARY KEY,
    actor        TEXT NOT NULL,
    class        TEXT NOT NULL,
    action       TEXT NOT NULL,
    target       TEXT NOT NULL,
    method       TEXT NOT NULL,
    path         TEXT NOT NULL,
    body         TEXT NOT NULL,
    sentence     TEXT NOT NULL,
    why          TEXT,
    created_at   REAL NOT NULL,
    expires_at   REAL NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending',
    decided_by   TEXT,
    decided_at   REAL,
    evidence_row INTEGER
);
CREATE INDEX IF NOT EXISTS idx_delegation_grants_status ON _amux_delegation_grants(status, created_at);
