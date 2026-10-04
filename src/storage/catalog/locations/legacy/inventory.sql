CREATE TABLE IF NOT EXISTS storage_legacy_recordings (
    recording_id TEXT PRIMARY KEY REFERENCES recording_files(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
    file_identity TEXT CHECK(length(file_identity) BETWEEN 1 AND 256),
    catalog_identity TEXT CHECK(length(catalog_identity) BETWEEN 1 AND 256),
    bytes INTEGER CHECK(bytes IS NULL OR (typeof(bytes)='integer' AND bytes>=0)),
    digest BLOB CHECK(length(digest)=32),
    CHECK((file_identity IS NULL AND catalog_identity IS NULL AND bytes IS NULL AND digest IS NULL)
        OR (file_identity IS NOT NULL AND catalog_identity IS NOT NULL AND bytes IS NOT NULL AND digest IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS storage_legacy_recording_page ON recording_files(id) WHERE finalized=1;
CREATE VIEW IF NOT EXISTS storage_legacy_recording_candidates AS
    SELECT r.id,r.path FROM recording_files r WHERE r.finalized=1 AND r.cleanup_pending=0
    AND NOT EXISTS(SELECT 1 FROM storage_volume_allocations a WHERE a.kind='recording' AND a.state!='cancelled'
        AND (a.object_id=r.id OR a.destination_path=replace(r.path,char(92),'/') COLLATE NOCASE))
    AND NOT EXISTS(SELECT 1 FROM recording_maintenance_claims m WHERE m.active=1
        AND (m.recording_id=r.id OR replace(m.path,char(92),'/')=replace(r.path,char(92),'/') COLLATE NOCASE));
CREATE TRIGGER IF NOT EXISTS storage_legacy_recording_changed
AFTER UPDATE ON recording_files
WHEN NEW.path IS NOT OLD.path OR NEW.file_bytes IS NOT OLD.file_bytes
    OR NEW.file_identity IS NOT OLD.file_identity OR NEW.finalized IS NOT OLD.finalized
    OR NEW.started_at_ms IS NOT OLD.started_at_ms OR NEW.ended_at_ms IS NOT OLD.ended_at_ms
    OR NEW.init_offset IS NOT OLD.init_offset OR NEW.init_len IS NOT OLD.init_len
    OR NEW.source_id IS NOT OLD.source_id OR NEW.stream_id IS NOT OLD.stream_id
    OR NEW.logical_stream_id IS NOT OLD.logical_stream_id
BEGIN
    UPDATE storage_legacy_recordings SET path=NEW.path,revision=revision+1,
        file_identity=NULL,catalog_identity=NULL,bytes=NULL,digest=NULL WHERE recording_id=OLD.id;
END;
