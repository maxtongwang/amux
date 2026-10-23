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

-- Dev-lead rulings 2026-10-23 (second B1 in the approval-reuse class):
-- one reply reserves one immutable change request, regardless of ask, actor,
-- target, route or mode, even on failure/undo. NULL ask_row (passkey) is NOT
-- exempt: claiming evidence later must obey the same global uniqueness.
-- ADDCOL: _amux_delegation_grants cite_spent_at REAL
-- ADDCOL: _amux_delegation_grants target_incarnation TEXT
-- ADDCOL: cmd_history cite_worker_ids TEXT
CREATE UNIQUE INDEX IF NOT EXISTS idx_delegation_cite_reply
    ON _amux_delegation_grants(evidence_row)
    WHERE evidence_row IS NOT NULL;
