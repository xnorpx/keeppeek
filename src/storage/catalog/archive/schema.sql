CREATE TABLE IF NOT EXISTS archive_bindings (
    id TEXT PRIMARY KEY,
    root TEXT NOT NULL,
    filesystem TEXT NOT NULL,
    root_identity TEXT NOT NULL,
    UNIQUE(filesystem,root_identity)
);
CREATE TABLE IF NOT EXISTS archive_replicas (
    job_id TEXT PRIMARY KEY,
    recording_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN('recording','export','probe')),
    target_id TEXT NOT NULL REFERENCES archive_bindings(id),
    object_id TEXT NOT NULL UNIQUE,
    source_path TEXT NOT NULL,
    source_identity TEXT NOT NULL,
    bytes INTEGER NOT NULL CHECK(typeof(bytes)='integer' AND bytes>0),
    digest BLOB NOT NULL CHECK(length(digest)=32),
    chunk_hashes BLOB NOT NULL DEFAULT X'',
    source_root TEXT NOT NULL,
    source_filesystem TEXT NOT NULL,
    source_root_identity TEXT NOT NULL,
    source_relative_key TEXT NOT NULL,
    source_operation TEXT NOT NULL,
    export_job_id TEXT UNIQUE,
    export_metadata TEXT CHECK(length(export_metadata)<=65536),
    stage_identity TEXT,
    stage_bytes INTEGER NOT NULL DEFAULT 0 CHECK(stage_bytes>=0 AND stage_bytes<=bytes),
    version TEXT,
    state TEXT NOT NULL CHECK(state IN('queued','copying','ready','failed','cancelled','restoring','restored','deleting','deleted')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts>=0),
    cancelled INTEGER NOT NULL DEFAULT 0 CHECK(cancelled IN(0,1)),
    local_evicted INTEGER NOT NULL DEFAULT 0 CHECK(local_evicted IN(0,1,2)),
    source_retirement_operation TEXT,
    action TEXT NOT NULL DEFAULT '' CHECK(action IN('','evict','restore')),
    action_attempts INTEGER NOT NULL DEFAULT 0 CHECK(action_attempts>=0),
    restore_operation TEXT,
    restore_identity TEXT,
    restore_bytes INTEGER NOT NULL DEFAULT 0 CHECK(restore_bytes>=0 AND restore_bytes<=bytes),
    probe_acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(probe_acknowledged IN(0,1)),
    cleanup_identity TEXT,
    cleanup_bytes INTEGER,
    cleanup_digest BLOB,
    cleanup_key TEXT,
    cleanup_complete INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_complete IN(0,1)),
    cleanup_acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_acknowledged IN(0,1)),
    target_reserved_bytes INTEGER NOT NULL DEFAULT 0 CHECK(target_reserved_bytes>=0),
    target_materialized_bytes INTEGER NOT NULL DEFAULT 0 CHECK(target_materialized_bytes>=0 AND target_materialized_bytes<=target_reserved_bytes),
    error TEXT CHECK(length(error)<=256),
    UNIQUE(kind,recording_id)
);
CREATE INDEX IF NOT EXISTS archive_replicas_state ON archive_replicas(state,job_id);
CREATE INDEX IF NOT EXISTS archive_replicas_pending_eviction
    ON archive_replicas(recording_id) WHERE kind='recording' AND local_evicted=2;
CREATE INDEX IF NOT EXISTS archive_transfer_reservations
    ON archive_replicas(target_id,job_id,target_reserved_bytes,target_materialized_bytes)
    WHERE target_reserved_bytes>target_materialized_bytes;
CREATE TRIGGER IF NOT EXISTS archive_eviction_fence_protection
BEFORE UPDATE OF protected ON recording_files
WHEN NEW.protected!=0 AND OLD.protected=0
    AND EXISTS(SELECT 1 FROM archive_replicas
               WHERE kind='recording' AND recording_id=OLD.id AND local_evicted=2)
BEGIN SELECT RAISE(ABORT,'archive eviction is in progress'); END;
CREATE TRIGGER IF NOT EXISTS archive_eviction_fence_retention_insert
BEFORE INSERT ON recording_retention_decisions
WHEN EXISTS(SELECT 1 FROM archive_replicas
            WHERE kind='recording' AND recording_id=NEW.recording_id AND local_evicted=2)
BEGIN SELECT RAISE(ABORT,'archive eviction is in progress'); END;
CREATE TRIGGER IF NOT EXISTS archive_eviction_fence_retention_update
BEFORE UPDATE OF policy_revision,policy_fingerprint,event_revision,deadline_ms,matching_rules_json,reason_json
ON recording_retention_decisions
WHEN EXISTS(SELECT 1 FROM archive_replicas
            WHERE kind='recording' AND recording_id=NEW.recording_id AND local_evicted=2)
BEGIN SELECT RAISE(ABORT,'archive eviction is in progress'); END;
