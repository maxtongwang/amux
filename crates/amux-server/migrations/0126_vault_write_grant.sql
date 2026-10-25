-- Standing configuration, not an approval request. All timestamps are Unix seconds.
-- Binding is filled ONCE at startup from the configured owner registry, never from a request.
CREATE TABLE IF NOT EXISTS vault_write_grants (
    id TEXT PRIMARY KEY,
    grantee TEXT NOT NULL,
    grantee_id TEXT,
    identity_kind TEXT,
    incarnation TEXT,
    scope TEXT NOT NULL CHECK (scope = 'global'),
    authority TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at REAL NOT NULL,
    revoked_at REAL
);
INSERT INTO vault_write_grants
    (id, grantee, scope, authority, created_by, created_at)
VALUES
    ('vault-owner-global-write-v1', 'configured_owner', 'global',
     'Max: "Grant vault writes"; mrpopo session, 2026-10-24 19:54:18 PDT; verified via amux-origin. Earlier context: <19:49 context>; <19:52 context>',
     'maxtongwang/amux PR <number>', 1792896858.0)
ON CONFLICT(id) DO NOTHING;
