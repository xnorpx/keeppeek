CREATE TABLE IF NOT EXISTS storage_volume_ledger (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (typeof(revision) = 'integer' AND revision >= 0),
    pending_count INTEGER NOT NULL DEFAULT 0 CHECK (typeof(pending_count) = 'integer' AND pending_count >= 0)
);
INSERT OR IGNORE INTO storage_volume_ledger(singleton, revision) VALUES (1, 0);
CREATE TABLE IF NOT EXISTS storage_volume_bindings (
    id TEXT PRIMARY KEY,
    generation INTEGER NOT NULL CHECK (generation > 0),
    root TEXT NOT NULL,
    filesystem TEXT NOT NULL,
    root_identity TEXT NOT NULL,
    writable INTEGER NOT NULL CHECK (writable IN (0, 1)),
    draining INTEGER NOT NULL DEFAULT 0 CHECK (draining IN (0, 1)),
    operator_draining INTEGER NOT NULL DEFAULT 0 CHECK (operator_draining IN (0, 1)),
    limit_bytes INTEGER CHECK (limit_bytes > 0),
    minimum_free_bytes INTEGER NOT NULL CHECK (minimum_free_bytes >= 0),
    allocated_bytes INTEGER NOT NULL DEFAULT 0 CHECK (typeof(allocated_bytes) = 'integer' AND allocated_bytes >= 0),
    reserved_bytes INTEGER NOT NULL DEFAULT 0 CHECK (typeof(reserved_bytes) = 'integer' AND reserved_bytes >= 0),
    UNIQUE(id, generation),
    UNIQUE(filesystem, root_identity)
);
CREATE TABLE IF NOT EXISTS storage_volume_allocations (
    operation TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('recording', 'export', 'thumbnail')),
    object_id TEXT NOT NULL,
    volume_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    relative_key TEXT NOT NULL COLLATE NOCASE,
    destination_path TEXT NOT NULL COLLATE NOCASE UNIQUE,
    bytes INTEGER NOT NULL CHECK (bytes > 0),
    intent_bytes INTEGER NOT NULL CHECK (intent_bytes > 0),
    materialized_bytes INTEGER NOT NULL DEFAULT 0 CHECK (typeof(materialized_bytes) = 'integer' AND materialized_bytes >= 0 AND materialized_bytes <= bytes),
    state TEXT NOT NULL CHECK (state IN ('reserved', 'published', 'cancelled')),
    file_identity TEXT,
    digest BLOB,
    location_revision INTEGER NOT NULL DEFAULT 0 CHECK (location_revision >= 0),
    FOREIGN KEY(volume_id, generation) REFERENCES storage_volume_bindings(id, generation),
    UNIQUE(kind, object_id),
    UNIQUE(volume_id, relative_key)
);
CREATE INDEX IF NOT EXISTS storage_volume_allocation_volume
    ON storage_volume_allocations(volume_id, state);
CREATE INDEX IF NOT EXISTS storage_volume_object_page
    ON storage_volume_allocations(volume_id, state, kind, object_id);
CREATE INDEX IF NOT EXISTS storage_volume_maintenance_path
    ON recording_maintenance_claims(replace(path, char(92), '/') COLLATE NOCASE) WHERE active = 1;
CREATE INDEX IF NOT EXISTS storage_volume_cleanup_path
    ON recording_files(replace(path, char(92), '/') COLLATE NOCASE) WHERE cleanup_pending = 1;
DROP TRIGGER IF EXISTS storage_volume_allocation_insert;
CREATE TRIGGER storage_volume_allocation_insert
AFTER INSERT ON storage_volume_allocations BEGIN
    UPDATE storage_volume_bindings
        SET allocated_bytes = allocated_bytes + CASE WHEN NEW.state != 'cancelled' THEN NEW.bytes ELSE 0 END,
            reserved_bytes = reserved_bytes + CASE WHEN NEW.state = 'reserved' THEN NEW.bytes - NEW.materialized_bytes ELSE 0 END
        WHERE id = NEW.volume_id;
    UPDATE storage_volume_ledger
        SET pending_count = pending_count + CASE WHEN NEW.state = 'reserved' THEN 1 ELSE 0 END
        WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS storage_volume_allocation_maintenance_fence
BEFORE INSERT ON storage_volume_allocations
WHEN NEW.kind = 'recording' AND (
    EXISTS (SELECT 1 FROM recording_maintenance_claims
        WHERE active = 1 AND (recording_id = NEW.object_id
            OR replace(path, char(92), '/') = NEW.destination_path COLLATE NOCASE))
    OR EXISTS (SELECT 1 FROM recording_files WHERE cleanup_pending = 1
        AND (id = NEW.object_id OR replace(path, char(92), '/') = NEW.destination_path COLLATE NOCASE))
)
BEGIN SELECT RAISE(ABORT, 'recording maintenance conflicts with allocation'); END;
CREATE TRIGGER IF NOT EXISTS storage_volume_maintenance_allocation_fence
BEFORE INSERT ON recording_maintenance_claims
WHEN EXISTS (SELECT 1 FROM storage_volume_allocations
    WHERE kind = 'recording' AND state != 'cancelled'
        AND (object_id = NEW.recording_id OR destination_path = replace(NEW.path, char(92), '/') COLLATE NOCASE))
AND NOT EXISTS (SELECT 1 FROM storage_volume_allocations a
    JOIN storage_volume_bindings b ON b.id=a.volume_id AND b.generation=a.generation
    WHERE a.operation=NEW.volume_operation AND a.kind='recording' AND a.state='published'
        AND a.object_id=NEW.recording_id AND a.bytes=NEW.file_bytes AND b.writable=1
        AND a.destination_path=replace(NEW.path,char(92),'/') COLLATE NOCASE
        AND NOT EXISTS (SELECT 1 FROM storage_volume_moves m WHERE m.kind='recording'
            AND m.object_id=NEW.recording_id AND (m.phase NOT IN ('complete','cancelled') OR m.receipt_acknowledged=0))
        AND NOT EXISTS (SELECT 1 FROM storage_recording_retirements r WHERE r.recording_id=NEW.recording_id AND r.acknowledged=0)
        AND NOT EXISTS (SELECT 1 FROM storage_recording_recovery r WHERE r.recording_id=NEW.recording_id AND r.complete=0))
BEGIN SELECT RAISE(ABORT, 'volume ownership requires volume maintenance'); END;
CREATE TRIGGER IF NOT EXISTS storage_volume_recording_update_fence
BEFORE UPDATE ON recording_files
WHEN EXISTS (SELECT 1 FROM storage_volume_allocations
    WHERE kind = 'recording' AND state != 'cancelled'
        AND (((NEW.cleanup_pending != OLD.cleanup_pending OR NEW.cleanup_pending = 1)
            AND (object_id = OLD.id OR destination_path = replace(OLD.path, char(92), '/') COLLATE NOCASE
                OR destination_path = replace(NEW.path, char(92), '/') COLLATE NOCASE))
            OR (object_id = OLD.id AND state = 'published' AND (NEW.path != OLD.path OR NEW.id != OLD.id
                OR NEW.finalized != OLD.finalized OR NEW.file_bytes != OLD.file_bytes
                OR NEW.file_identity IS NOT OLD.file_identity))))
BEGIN SELECT RAISE(ABORT, 'volume ownership requires a location transition'); END;
CREATE TRIGGER IF NOT EXISTS storage_volume_recording_delete_fence
BEFORE DELETE ON recording_files
WHEN EXISTS (SELECT 1 FROM storage_volume_allocations
    WHERE kind = 'recording' AND state != 'cancelled'
        AND (object_id = OLD.id OR destination_path = replace(OLD.path, char(92), '/') COLLATE NOCASE))
BEGIN SELECT RAISE(ABORT, 'volume ownership requires volume maintenance'); END;
DROP TRIGGER IF EXISTS storage_volume_allocation_update;
CREATE TRIGGER storage_volume_allocation_update
AFTER UPDATE ON storage_volume_allocations BEGIN
    UPDATE storage_volume_bindings
        SET allocated_bytes = allocated_bytes - CASE WHEN OLD.state != 'cancelled' THEN OLD.bytes ELSE 0 END,
            reserved_bytes = reserved_bytes - CASE WHEN OLD.state = 'reserved' THEN OLD.bytes - OLD.materialized_bytes ELSE 0 END
        WHERE id = OLD.volume_id;
    UPDATE storage_volume_bindings
        SET allocated_bytes = allocated_bytes + CASE WHEN NEW.state != 'cancelled' THEN NEW.bytes ELSE 0 END,
            reserved_bytes = reserved_bytes + CASE WHEN NEW.state = 'reserved' THEN NEW.bytes - NEW.materialized_bytes ELSE 0 END
        WHERE id = NEW.volume_id;
    UPDATE storage_volume_ledger
        SET pending_count = pending_count - CASE WHEN OLD.state = 'reserved' THEN 1 ELSE 0 END
            + CASE WHEN NEW.state = 'reserved' THEN 1 ELSE 0 END
        WHERE singleton = 1;
END;
DROP TRIGGER IF EXISTS storage_volume_allocation_delete;
CREATE TRIGGER storage_volume_allocation_delete
AFTER DELETE ON storage_volume_allocations BEGIN
    UPDATE storage_volume_bindings
        SET allocated_bytes = allocated_bytes - CASE WHEN OLD.state != 'cancelled' THEN OLD.bytes ELSE 0 END,
            reserved_bytes = reserved_bytes - CASE WHEN OLD.state = 'reserved' THEN OLD.bytes - OLD.materialized_bytes ELSE 0 END
        WHERE id = OLD.volume_id;
    UPDATE storage_volume_ledger
        SET pending_count = pending_count - CASE WHEN OLD.state = 'reserved' THEN 1 ELSE 0 END
        WHERE singleton = 1;
END;
