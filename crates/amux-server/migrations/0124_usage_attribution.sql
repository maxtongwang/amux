-- Response provenance is additive; no historical counters or identities are rewritten.
-- ADDCOL: token_ledger worker_id TEXT
-- ADDCOL: token_ledger attribution TEXT
-- ADDCOL: token_ledger parent_conversation TEXT
-- ADDCOL: token_ledger compaction TEXT
CREATE INDEX IF NOT EXISTS idx_ledger_worker_time ON token_ledger(worker_id, ts, id);
CREATE INDEX IF NOT EXISTS idx_ledger_conversation_time ON token_ledger(conversation, ts, id);
CREATE TABLE IF NOT EXISTS usage_work_snapshots (
    worker_id TEXT NOT NULL,
    conversation TEXT NOT NULL,
    effective_at INTEGER NOT NULL,
    turn_id TEXT NOT NULL DEFAULT '',
    attribution TEXT NOT NULL,
    PRIMARY KEY(worker_id, conversation, effective_at, turn_id)
);
CREATE INDEX IF NOT EXISTS idx_usage_snapshot_conversation
    ON usage_work_snapshots(conversation, effective_at);
CREATE INDEX IF NOT EXISTS idx_ledger_work_time ON token_ledger(
    json_extract(attribution, '$.primary.kind'),
    json_extract(attribution, '$.primary.id'), ts, id
);
-- Legacy workers already have an immutable CC_INCARNATION. Retain observed
-- aliases in the ledger dimension without creating a second worker lifecycle.
CREATE TABLE IF NOT EXISTS usage_worker_identities (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    name_aliases TEXT NOT NULL DEFAULT '[]',
    environment TEXT NOT NULL DEFAULT '{}',
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL
);
CREATE VIEW IF NOT EXISTS usage_workers AS
    SELECT id,display_name,name_aliases,environment FROM _amux_workers
    UNION ALL
    SELECT id,display_name,name_aliases,environment FROM usage_worker_identities;
