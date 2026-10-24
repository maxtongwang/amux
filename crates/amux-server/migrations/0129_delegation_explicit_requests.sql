-- Max 2026-10-23: approval identity resolution (refs maxtongwang/ai-config#160).
-- Existing grant codes are explicit request IDs; old grants have NULL bindings
-- and cannot acquire fresh approval. Times are Unix seconds; cmd_history.ts is ms.
-- ADDCOL: _amux_delegation_grants actor_incarnation TEXT
-- ADDCOL: _amux_delegation_grants reply_session TEXT
-- ADDCOL: _amux_delegation_grants reply_incarnation TEXT
-- ADDCOL: _amux_delegation_grants shown_row INTEGER
-- ADDCOL: _amux_delegation_grants change_spec TEXT
-- ADDCOL: _amux_delegation_grants resolution_row INTEGER
-- ADDCOL: _amux_delegation_grants resolution_code TEXT
-- ADDCOL: _amux_delegation_grants resolution_note TEXT
-- ADDCOL: cmd_history delegation_session_incarnation TEXT
-- Preserve authorship when project execution projects type=user to steering.
-- ADDCOL: cmd_history delegation_message_type TEXT
CREATE INDEX IF NOT EXISTS idx_delegation_request_session
    ON _amux_delegation_grants(reply_session, shown_row) WHERE status='pending';
CREATE UNIQUE INDEX IF NOT EXISTS idx_delegation_request_shown
    ON _amux_delegation_grants(shown_row) WHERE shown_row IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_delegation_request_reply
    ON _amux_delegation_grants(resolution_row) WHERE resolution_row IS NOT NULL;
-- Durable decisions include refusals with no request, so a zero-candidate yes
-- is observable. A reply is resolved once, even across restart or later asks.
CREATE TABLE IF NOT EXISTS _amux_delegation_resolutions (
    reply_row INTEGER PRIMARY KEY,
    request_id TEXT UNIQUE,
    session TEXT NOT NULL,
    code TEXT NOT NULL,
    note TEXT NOT NULL,
    created_at REAL NOT NULL
);
