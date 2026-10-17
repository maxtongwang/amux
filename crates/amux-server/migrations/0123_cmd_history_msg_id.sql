-- 0123_cmd_history_msg_id.sql: the send's message ID on its history row.
--
-- A send's receipt is decided ONLY by its unique message ID (dev-lead
-- 2026-10-17, after three review rounds of PR #9 each found a receipt hole in
-- text or time-window matching). The send handler writes the request's msg_id
-- onto the row it records; the receipt read matches that column and nothing
-- else. NULL on every existing row: a row without an ID never confirms a send.
-- ADDCOL: cmd_history msg_id TEXT
CREATE INDEX IF NOT EXISTS idx_cmd_history_session_msg_id ON cmd_history(session, msg_id);
