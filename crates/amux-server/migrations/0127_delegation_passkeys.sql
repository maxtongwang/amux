-- Public credentials and server-only, single-use WebAuthn state. No private key leaves the authenticator.
CREATE TABLE _amux_delegation_passkeys (
    id TEXT PRIMARY KEY,
    rp_id TEXT NOT NULL,
    credential TEXT NOT NULL,
    counter INTEGER NOT NULL DEFAULT 0,
    created_at REAL NOT NULL,
    modified_at REAL NOT NULL,
    revoked_at REAL
);
CREATE TABLE _amux_delegation_ceremonies (
    id TEXT PRIMARY KEY,
    purpose TEXT NOT NULL,
    origin TEXT NOT NULL,
    state TEXT NOT NULL,
    expires_at REAL NOT NULL
);
