-- Affected pre-runtime table definitions from b877bdd674c32dc1f84e92356f9c35d67197ea14.

CREATE TABLE IF NOT EXISTS recording_files (
                 id TEXT PRIMARY KEY,
                 stream_id TEXT NOT NULL,
                 source_id TEXT,
                 logical_stream_id TEXT,
                 started_at_ms INTEGER NOT NULL,
                 ended_at_ms INTEGER,
                 path TEXT NOT NULL UNIQUE,
                 init_offset INTEGER NOT NULL,
                 init_len INTEGER NOT NULL,
                 finalized INTEGER NOT NULL,
                 finalized_at_ms INTEGER,
                 file_identity TEXT,
                 file_bytes INTEGER NOT NULL DEFAULT 0,
                 protected INTEGER NOT NULL DEFAULT 0,
                 cleanup_pending INTEGER NOT NULL DEFAULT 0
             );

CREATE TABLE IF NOT EXISTS recording_fragments (
                 recording_id TEXT NOT NULL REFERENCES recording_files(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 start_ms INTEGER NOT NULL,
                 duration_ms INTEGER NOT NULL,
                 byte_offset INTEGER NOT NULL,
                 byte_len INTEGER NOT NULL,
                 random_access INTEGER NOT NULL,
                 PRIMARY KEY(recording_id, sequence)
             );

CREATE TABLE IF NOT EXISTS recording_keyframes (
                 recording_id TEXT NOT NULL,
                 fragment_sequence INTEGER NOT NULL,
                 byte_offset INTEGER NOT NULL,
                 byte_len INTEGER NOT NULL,
                 PRIMARY KEY(recording_id, fragment_sequence),
                 FOREIGN KEY(recording_id, fragment_sequence)
                     REFERENCES recording_fragments(recording_id, sequence) ON DELETE CASCADE
             );

CREATE TABLE IF NOT EXISTS recording_events (
                 id TEXT PRIMARY KEY,
                 revision INTEGER NOT NULL DEFAULT 1,
                 publication_id TEXT,
                 publication_fingerprint TEXT,
                 camera_id TEXT NOT NULL,
                 stream TEXT,
                 source TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 start_time_ms INTEGER NOT NULL,
                 end_time_ms INTEGER,
                 confidence REAL,
                 bbox_json TEXT,
                 bbox_attachment_id TEXT,
                 zone TEXT,
                 text TEXT,
                 payload_json TEXT,
                 attachments_json TEXT NOT NULL DEFAULT '[]',
                 canonical_attachment_id TEXT,
                 icon_key TEXT NOT NULL DEFAULT 'event',
                 rejected_icon_key TEXT,
                 thumbnail_filename TEXT,
                 search_revision INTEGER NOT NULL DEFAULT 0
             );

CREATE TABLE IF NOT EXISTS recording_retention_decisions (
             recording_id TEXT PRIMARY KEY REFERENCES recording_files(id) ON DELETE CASCADE,
             policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
             policy_fingerprint BLOB NOT NULL CHECK (length(policy_fingerprint) = 32),
             event_revision INTEGER NOT NULL CHECK (event_revision >= 0),
             deadline_ms INTEGER,
             matching_rules_json TEXT NOT NULL CHECK (length(matching_rules_json) <= 4096),
             reason_json TEXT NOT NULL CHECK (length(reason_json) <= 64)
         );
