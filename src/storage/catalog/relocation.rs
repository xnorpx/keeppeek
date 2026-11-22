//! Durable ownership across publication, catalog handoff and original-file retirement.
use super::{BUSY_TIMEOUT, Command, RecordingCatalogHandle};
use anyhow::{Result, ensure};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

pub struct Move {
    pub id: String,
    pub stream: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub staged: uuid::Uuid,
    pub source_identity: String,
    pub destination_published: bool,
}

pub(super) enum Request {
    Reserve(Move),
    Next(String),
    Retire(String, uuid::Uuid),
}

pub(super) async fn initialize(db: &turso::Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS legacy_recording_relocations(
        id TEXT PRIMARY KEY REFERENCES recording_files(id) ON DELETE RESTRICT,
        stream TEXT NOT NULL, source TEXT NOT NULL UNIQUE, destination TEXT NOT NULL UNIQUE,
        stage TEXT NOT NULL UNIQUE, source_identity TEXT NOT NULL);
        CREATE TRIGGER IF NOT EXISTS legacy_relocation_path_insert BEFORE INSERT ON recording_files
        WHEN EXISTS(SELECT 1 FROM legacy_recording_relocations WHERE id!=NEW.id AND (source=NEW.path OR destination=NEW.path))
        BEGIN SELECT RAISE(ABORT,'recording location is reserved'); END;
        CREATE TRIGGER IF NOT EXISTS legacy_relocation_path_update BEFORE UPDATE OF path ON recording_files
        WHEN EXISTS(SELECT 1 FROM legacy_recording_relocations WHERE (id=NEW.id AND source!=NEW.path AND destination!=NEW.path)
          OR (id!=NEW.id AND (source=NEW.path OR destination=NEW.path)))
        BEGIN SELECT RAISE(ABORT,'recording location is reserved'); END;
        CREATE TRIGGER IF NOT EXISTS legacy_relocation_delete BEFORE DELETE ON recording_files
        WHEN EXISTS(SELECT 1 FROM legacy_recording_relocations WHERE id=OLD.id)
        BEGIN SELECT RAISE(ABORT,'recording relocation is pending'); END;
        CREATE TRIGGER IF NOT EXISTS legacy_relocation_cleanup BEFORE UPDATE OF cleanup_pending ON recording_files
        WHEN NEW.cleanup_pending=1 AND EXISTS(SELECT 1 FROM legacy_recording_relocations WHERE id=NEW.id)
        BEGIN SELECT RAISE(ABORT,'recording relocation is pending'); END;
        CREATE TRIGGER IF NOT EXISTS legacy_relocation_maintenance BEFORE INSERT ON recording_maintenance_claims
        WHEN EXISTS(SELECT 1 FROM legacy_recording_relocations WHERE id=NEW.recording_id OR source=NEW.path OR destination=NEW.path)
        BEGIN SELECT RAISE(ABORT,'recording relocation is pending'); END;").await?;
    Ok(())
}

/// A journal entry pins both paths and the original filesystem identity until handoff finishes.
pub fn ensure_migration_idle(db: &turso::Connection) -> Result<()> {
    pollster::block_on(async {
        let mut schema = db.query(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='legacy_recording_relocations'",
            (),
        ).await?;
        if schema.next().await?.is_some() {
            drop(schema);
            let mut pending = db
                .query("SELECT 1 FROM legacy_recording_relocations LIMIT 1", ())
                .await?;
            ensure!(
                pending.next().await?.is_none(),
                "finish pending recording relocations before moving storage"
            );
        }
        Ok(())
    })
}

fn read(row: &turso::Row) -> Result<Move> {
    Ok(Move {
        id: row.get(0)?,
        stream: row.get(1)?,
        source: PathBuf::from(row.get::<String>(2)?),
        destination: PathBuf::from(row.get::<String>(3)?),
        staged: row.get::<String>(4)?.parse()?,
        source_identity: row.get(5)?,
        destination_published: row.get::<i64>(6)? != 0,
    })
}

const COLUMNS: &str = "id,stream,source,destination,stage,source_identity,
    EXISTS(SELECT 1 FROM recording_files f WHERE f.id=legacy_recording_relocations.id AND f.path=legacy_recording_relocations.destination)";

pub(super) async fn dispatch(db: &turso::Connection, request: Request) -> Result<Option<Move>> {
    match request {
        Request::Next(after) => {
            let mut rows=db.query(format!("SELECT {COLUMNS} FROM legacy_recording_relocations WHERE id>?1 ORDER BY id LIMIT 1"),[after]).await?;
            rows.next().await?.as_ref().map(read).transpose()
        }
        Request::Reserve(job) => {
            let path = |p: &Path| {
                p.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("invalid recording path"))
            };
            let source = path(&job.source)?;
            let destination = path(&job.destination)?;
            db.execute("INSERT INTO legacy_recording_relocations(id,stream,source,destination,stage,source_identity)
                  SELECT id,stream_id,?2,?3,?4,?5 FROM recording_files
                  WHERE id=?1 AND path IN (?2,?3) AND stream_id=?6 AND finalized=1 AND cleanup_pending=0
                  AND (path=?3 OR file_identity IS NULL OR file_identity=?5)
                  AND NOT EXISTS(SELECT 1 FROM recording_files WHERE id!=?1 AND path IN (?2,?3))
                  AND NOT EXISTS(SELECT 1 FROM recording_maintenance_claims WHERE active=1 AND (recording_id=?1 OR path IN (?2,?3)))
                  ON CONFLICT(id) DO NOTHING",turso::params![job.id.as_str(),source,destination,job.staged.to_string(),job.source_identity.as_str(),job.stream.as_str()]).await?;
            let mut rows = db
                .query(
                    format!("SELECT {COLUMNS} FROM legacy_recording_relocations WHERE id=?1"),
                    [job.id],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("recording is unavailable for relocation"))?;
            let existing = read(&row)?;
            ensure!(
                existing.source == job.source
                    && existing.destination == job.destination
                    && existing.source_identity == job.source_identity,
                "recording relocation identity changed"
            );
            Ok(Some(existing))
        }
        Request::Retire(id, stage) => {
            ensure!(
                db.execute(
                    "DELETE FROM legacy_recording_relocations WHERE id=?1 AND stage=?2",
                    (id, stage.to_string())
                )
                .await?
                    == 1,
                "recording relocation ownership changed"
            );
            Ok(None)
        }
    }
}

impl RecordingCatalogHandle {
    fn relocation(&self, request: Request) -> Result<Option<Move>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::Relocation { request, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog unavailable"))?;
        response
            .recv_timeout(BUSY_TIMEOUT)
            .map_err(|_| anyhow::anyhow!("recording relocation reply unavailable"))?
    }

    pub(crate) fn reserve_relocation(&self, job: Move) -> Result<Move> {
        self.relocation(Request::Reserve(job))?
            .ok_or_else(|| anyhow::anyhow!("missing recording relocation"))
    }
    pub(crate) fn next_relocation(&self, after: &str) -> Result<Option<Move>> {
        self.relocation(Request::Next(after.to_owned()))
    }
    pub(crate) fn retire_relocation(&self, job: &Move) -> Result<()> {
        self.relocation(Request::Retire(job.id.clone(), job.staged))?;
        Ok(())
    }
}
