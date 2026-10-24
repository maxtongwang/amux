-- Max 2026-10-24 12:48: session-based approval, refs maxtongwang/ai-config#160.
-- NULL on legacy/API rows is never native funding proof.
-- ADDCOL: cmd_history approval_native_session TEXT
-- ADDCOL: cmd_history approval_wrapped INTEGER
-- v3.1 short display codes; all *_at timestamps are Unix seconds (REAL).
-- Native hook sequence is the ordering authority; cmd_history.ts remains ms.
-- ADDCOL: cmd_history approval_run TEXT
-- ADDCOL: cmd_history approval_sequence INTEGER
-- ADDCOL: _amux_delegation_grants short_code TEXT COLLATE NOCASE
-- ADDCOL: _amux_delegation_grants approval_closed_at REAL
-- ADDCOL: _amux_delegation_grants shown_snapshot TEXT
-- ADDCOL: _amux_delegation_grants confirm_required INTEGER NOT NULL DEFAULT 0
-- v3.2: first confirmation clock and durable once-only reminder receipts.
-- ADDCOL: _amux_delegation_grants confirm_needed_at REAL
-- ADDCOL: _amux_delegation_grants confirm_reshow_at REAL
-- ADDCOL: _amux_delegation_grants confirm_reshow_notice_at REAL
-- ADDCOL: _amux_delegation_grants confirm_reshow_push_at REAL
-- ADDCOL: _amux_delegation_grants confirm_reshow_push_result TEXT
-- ADDCOL: _amux_delegation_resolutions approval_context TEXT
CREATE UNIQUE INDEX IF NOT EXISTS idx_approval_open_short_code
    ON _amux_delegation_grants(short_code COLLATE NOCASE)
    WHERE approval_closed_at IS NULL AND short_code IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_approval_code_reuse
    ON _amux_delegation_grants(short_code, approval_closed_at);
CREATE TRIGGER IF NOT EXISTS approval_close_status AFTER UPDATE OF status ON _amux_delegation_grants
WHEN NEW.status <> 'pending' AND NEW.approval_closed_at IS NULL
BEGIN
    UPDATE _amux_delegation_grants SET approval_closed_at=unixepoch('now') WHERE code=NEW.code;
END;
