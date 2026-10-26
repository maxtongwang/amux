-- Upgrade existing dark ledgers without discarding ambiguous attempts.
DROP TRIGGER native_delivery_retain_pending;
DROP TRIGGER native_delivery_immutable_body;
DROP VIEW native_delivery_pending_queue;
CREATE TABLE native_delivery_attempts_v2 (
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
    state TEXT NOT NULL CHECK (state IN ('reserved','written_unconfirmed','held','admitted','consumed','denied','refused','expired','dropped','unknown','uncertain')),
    reason TEXT NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    write_attempted_at REAL,
    evidence_at REAL,
    finalized_at REAL,
    next_observation_at REAL NOT NULL DEFAULT 0,
    UNIQUE(session,source_key)
);
INSERT INTO native_delivery_attempts_v2 SELECT * FROM native_delivery_attempts;
DROP TABLE native_delivery_attempts;
ALTER TABLE native_delivery_attempts_v2 RENAME TO native_delivery_attempts;
ALTER TABLE native_delivery_attempts ADD COLUMN unknown_since REAL;
ALTER TABLE native_delivery_attempts ADD COLUMN observation_count INTEGER NOT NULL DEFAULT 0;
UPDATE native_delivery_attempts SET unknown_since=updated_at WHERE state='unknown';
CREATE INDEX native_delivery_queue ON native_delivery_attempts(queue_id);
CREATE INDEX native_delivery_message ON native_delivery_attempts(session,message_id);
CREATE INDEX native_delivery_observation ON native_delivery_attempts(state,next_observation_at);
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
