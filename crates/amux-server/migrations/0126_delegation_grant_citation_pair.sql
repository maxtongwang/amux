-- 0126_delegation_grant_citation_pair.sql: a cite-mode grant stores BOTH halves
-- of its citation (maxtongwang/ai-config#160, PR #18 review B4).
--
-- Ruling (dev-lead, 2026-10-19): a citation is the PAIR of (1) the asking
-- message the requesting agent sent into the cited session, naming the target
-- and the setting in plain words, and (2) Max's own FIRST typed message in
-- that session after it, within 2 hours. The grant keeps the ask's row id and
-- both texts so the audit shows what Max answered and what he was asked.
-- `evidence_row` and `cited_excerpt` (0125) keep the reply's row and excerpt.
-- NULL on every passkey-mode grant and on rows written before 0126.
-- ADDCOL: _amux_delegation_grants ask_row INTEGER
-- ADDCOL: _amux_delegation_grants ask_text TEXT
-- ADDCOL: _amux_delegation_grants reply_text TEXT

-- Round 3: one pair reserves one immutable change request, even if applying
-- it fails or the server crashes. Undo never releases evidence.
-- ADDCOL: _amux_delegation_grants cite_spent_at REAL
-- ADDCOL: _amux_delegation_grants target_incarnation TEXT
-- ADDCOL: cmd_history cite_worker_ids TEXT
CREATE UNIQUE INDEX IF NOT EXISTS idx_delegation_cite_pair
    ON _amux_delegation_grants(ask_row, evidence_row)
    WHERE ask_row IS NOT NULL AND evidence_row IS NOT NULL;
