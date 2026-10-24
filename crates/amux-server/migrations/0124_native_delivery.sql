-- Native writes are attempts, never implied delivery receipts. Keep ambiguous
-- attempts even across process death; a unique source prevents replay.
CREATE TABLE native_delivery_attempts (
    attempt_id TEXT PRIMARY KEY,
    session TEXT NOT NULL,
    source_key TEXT NOT NULL,
    message_id TEXT,
    queue_id TEXT,
    card_id TEXT,
    sender TEXT NOT NULL,
    transport TEXT NOT NULL DEFAULT 'claude_uds_v1',
    msg_id TEXT NOT NULL UNIQUE,
    command_uuid TEXT NOT NULL UNIQUE,
    endpoint TEXT NOT NULL,
    endpoint_pid INTEGER NOT NULL,
    endpoint_uid INTEGER NOT NULL,
    endpoint_start TEXT NOT NULL,
    endpoint_session TEXT NOT NULL,
    executable_sha256 TEXT NOT NULL,
    registry_sha256 TEXT NOT NULL,
    relay_pid INTEGER NOT NULL,
    relay_start TEXT NOT NULL,
    body_sha256 TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('reserved','written_unconfirmed','held','admitted','consumed','denied','refused','expired','dropped','unknown')),
    reason TEXT NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    write_attempted_at REAL,
    evidence_at REAL,
    finalized_at REAL,
    next_observation_at REAL NOT NULL DEFAULT 0,
    UNIQUE(session,source_key)
);
CREATE INDEX native_delivery_queue ON native_delivery_attempts(queue_id);
CREATE INDEX native_delivery_message ON native_delivery_attempts(session,message_id);
CREATE INDEX native_delivery_observation ON native_delivery_attempts(state,next_observation_at);
CREATE TABLE native_delivery_quarantine (
    endpoint_key TEXT PRIMARY KEY,
    reason TEXT NOT NULL,
    created_at REAL NOT NULL
);
-- Only the authenticated send boundary can opt a queued message into native
-- delivery. Historic/body-asserted queue senders are not proof of identity.
ALTER TABLE steering_queue ADD COLUMN native_sender TEXT;
ALTER TABLE steering_queue ADD COLUMN native_message_id TEXT;
ALTER TABLE steering_queue ADD COLUMN native_body_stamped INTEGER NOT NULL DEFAULT 0;
CREATE VIEW native_delivery_pending_queue AS
    SELECT q.id,q.session,q.queued_at FROM native_delivery_attempts n
    JOIN steering_queue q ON q.id=n.queue_id
    WHERE n.state IN ('reserved','written_unconfirmed','held','unknown');
-- No legacy cleanup, cancellation or inferred receipt may erase an
-- ambiguous native write. The protocol has no cancellation guarantee.
CREATE TRIGGER native_delivery_retain_pending BEFORE DELETE ON steering_queue
WHEN EXISTS (SELECT 1 FROM native_delivery_pending_queue p WHERE p.id=OLD.id)
BEGIN
    SELECT RAISE(ABORT,'native delivery remains pending; deletion cannot cancel Claude delivery');
END;
CREATE TRIGGER native_delivery_immutable_body BEFORE UPDATE OF text ON steering_queue
WHEN NEW.text<>OLD.text AND EXISTS (SELECT 1 FROM native_delivery_attempts n WHERE n.queue_id=OLD.id)
BEGIN
    SELECT RAISE(ABORT,'native delivery body is immutable after attempt reservation');
END;
