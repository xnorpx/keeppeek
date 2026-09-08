//! Reports catalog and filesystem drift for a confirmed, non-executing deletion job.
//!
//! Catalog inputs share one read transaction. Filesystem observations follow that transaction
//! and are not atomic with it or with one another. Reports neither reserve files nor prove
//! content identity or deletion ownership. A missing catalog row leaves the file location unknown.
//! Present files still require current authorization, evidence checks, and race-safe removal.

use super::{Action, Failure, Job, State, load, validate_action};
use crate::storage::catalog::maintenance::{
    MAX_RECORDINGS, ReadRequest, Recording, Scope, check_deadline,
};
use crate::storage::catalog::{BUSY_TIMEOUT, RecordingCatalogHandle, SearchCommand};
use crate::storage::long_term::inspection::{
    Archive, IDENTITY_BYTES_MAX, Identity, PATH_BYTES_MAX,
};
use std::{io::ErrorKind, path::PathBuf, sync::mpsc, time::Instant};

/// Describes one observation or blocker, never a successful deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Catalog identifiers and size match observed metadata; immutable content ownership remains unproven.
    Present,
    IdentityChanged,
    IdentityUnavailable,
    MissingFile,
    /// No trusted path remains; the report makes no claim about whether a file exists.
    MissingCatalog,
    CatalogChanged,
    ActiveRecording,
    ProtectedRecording,
    CleanupPending,
    SizeMismatch,
    PathRejected,
    InspectionFailed,
}

/// Associates a dry-run observation with one confirmed recording identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub recording_id: String,
    pub status: Status,
}

/// Contains a complete, bounded dry run without host paths or deletion authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub job_id: String,
    pub planned_revision: u64,
    /// Identifies the catalog read, not later filesystem state or current authorization.
    pub catalog_revision: u64,
    pub objects: Vec<Object>,
}

pub(in crate::storage::catalog) struct Inputs {
    job_id: String,
    planned_revision: u64,
    catalog_revision: u64,
    entries: Vec<(String, Check)>,
}

enum Check {
    Catalog(Status),
    File {
        path: PathBuf,
        bytes: u64,
        identity: Option<Identity>,
    },
}

impl RecordingCatalogHandle {
    /// Inspects a confirmed job's current catalog rows and archive metadata without modifying them.
    ///
    /// The caller must authenticate and authorize the Administrator and supply a trusted archive.
    /// The job must belong to that actor and remain queued at the catalog read. Work uses the
    /// search worker, at most 128 files, and one cooperative two-second deadline. Filesystem I/O
    /// runs on the caller after the read transaction closes. A changed catalog revision requires
    /// another preview; even a matching revision and all-present report do not authorize deletion.
    ///
    /// # Errors
    /// Rejects malformed identities, wrong owners, nonqueued or corrupt jobs, unavailable queues,
    /// and expired work. Timeouts return no partial report. Individual file failures remain visible.
    pub fn recording_deletion_preflight(
        &self,
        actor: impl AsRef<str>,
        id: impl AsRef<str>,
        archive: &Archive,
    ) -> anyhow::Result<Report> {
        let actor = actor.as_ref();
        let id = id.as_ref();
        anyhow::ensure!(id.len() == 32, Failure::Invalid);
        validate_action(actor, &Action::Read { id: id.to_owned() })?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .try_send(SearchCommand::Maintenance(ReadRequest::Preflight {
                actor: actor.to_owned(),
                id: id.to_owned(),
                deadline,
                reply,
            }))
            .map_err(|_| Failure::Unavailable)?;
        let inputs = response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::Unavailable)?
            .map_err(redact)?;
        Ok(inspect(inputs, archive, deadline).map_err(redact)?)
    }
}

fn redact(error: anyhow::Error) -> Failure {
    error
        .downcast_ref::<Failure>()
        .copied()
        .unwrap_or(Failure::Unavailable)
}

fn inspect(inputs: Inputs, archive: &Archive, deadline: Instant) -> anyhow::Result<Report> {
    let mut objects = Vec::with_capacity(inputs.entries.len());
    for (recording_id, check) in inputs.entries {
        check_deadline(deadline)?;
        let status = match check {
            Check::Catalog(status) => status,
            Check::File {
                path,
                bytes,
                identity,
            } => match archive.inspect_until(&path, bytes, deadline) {
                Ok(observation) => match identity {
                    Some(expected) if expected == observation.identity() => Status::Present,
                    Some(_) => Status::IdentityChanged,
                    None => Status::IdentityUnavailable,
                },
                Err(error) => match error.kind() {
                    ErrorKind::NotFound => Status::MissingFile,
                    ErrorKind::InvalidData => Status::SizeMismatch,
                    ErrorKind::PermissionDenied => Status::PathRejected,
                    ErrorKind::TimedOut => return Err(Failure::Unavailable.into()),
                    _ => Status::InspectionFailed,
                },
            },
        };
        objects.push(Object {
            recording_id,
            status,
        });
    }
    check_deadline(deadline)?;
    Ok(Report {
        job_id: inputs.job_id,
        planned_revision: inputs.planned_revision,
        catalog_revision: inputs.catalog_revision,
        objects,
    })
}

pub(in crate::storage::catalog) async fn read(
    connection: &turso::Connection,
    actor: &str,
    id: &str,
    deadline: Instant,
) -> anyhow::Result<Inputs> {
    check_deadline(deadline)?;
    connection.execute_batch("BEGIN").await?;
    let result = async {
        let inputs = read_inputs(connection, actor, id, deadline).await?;
        check_deadline(deadline)?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(inputs)
    }
    .await;
    if let Err(error) = &result {
        let autocommit = connection.is_autocommit().map_err(|state| {
            anyhow::anyhow!(
                "deletion preflight failed: {error}; transaction state unavailable: {state}"
            )
        })?;
        if !autocommit {
            connection
                .execute_batch("ROLLBACK")
                .await
                .map_err(|rollback| {
                    anyhow::anyhow!(
                        "deletion preflight failed: {error}; rollback failed: {rollback}"
                    )
                })?;
        }
    }
    result
}

async fn read_inputs(
    connection: &turso::Connection,
    actor: &str,
    id: &str,
    deadline: Instant,
) -> anyhow::Result<Inputs> {
    let (job, _) = load(connection, actor, id, deadline).await?;
    anyhow::ensure!(job.state == State::Queued, Failure::InvalidState);
    job.snapshot.scope.validate()?;
    let mut revisions = connection
        .query(
            "SELECT revision FROM recording_catalog_state WHERE id = 1",
            (),
        )
        .await?;
    let revision = revisions
        .next()
        .await?
        .ok_or(Failure::Invalid)?
        .get::<i64>(0)?;
    let catalog_revision = u64::try_from(revision).map_err(|_| Failure::Invalid)?;
    drop(revisions);
    check_deadline(deadline)?;
    let mut rows = current_rows(connection, &job).await?;
    let mut entries = Vec::with_capacity(job.objects.len());
    while let Some(row) = rows.next().await? {
        check_deadline(deadline)?;
        let expected = job
            .snapshot
            .recordings
            .get(entries.len())
            .ok_or(Failure::Invalid)?;
        anyhow::ensure!(
            row.get::<i64>(0)? == i64::try_from(entries.len())?,
            Failure::Invalid
        );
        entries.push((expected.recording_id.clone(), current(&row, expected)?));
    }
    anyhow::ensure!(entries.len() == job.objects.len(), Failure::Invalid);
    Ok(Inputs {
        job_id: job.id,
        planned_revision: job.revision,
        catalog_revision,
        entries,
    })
}

async fn current_rows(connection: &turso::Connection, job: &Job) -> anyhow::Result<turso::Rows> {
    let (source_id, stream_id) = match &job.snapshot.scope {
        Scope::Recording {
            source_id,
            stream_id,
            ..
        }
        | Scope::TimeRange {
            source_id,
            stream_id,
            ..
        } => (source_id, stream_id),
    };
    Ok(connection
        .query(
            "SELECT objects.ordinal, files.id IS NOT NULL,
            files.source_id = ?4 AND files.logical_stream_id = ?5,
            files.started_at_ms, files.ended_at_ms, files.file_bytes,
            files.finalized, files.protected, files.cleanup_pending,
            CASE WHEN length(CAST(files.path AS BLOB)) <= ?3 THEN files.path END,
            CASE WHEN length(CAST(files.file_identity AS BLOB)) > ?6 THEN '' ELSE files.file_identity END
         FROM recording_maintenance_objects AS objects
         LEFT JOIN recording_files AS files ON files.id = objects.recording_id
         WHERE objects.job_id = ?1 ORDER BY objects.ordinal LIMIT ?2",
            turso::params![
                job.id.as_str(),
                i64::try_from(MAX_RECORDINGS + 1)?,
                i64::try_from(PATH_BYTES_MAX)?,
                source_id.as_str(),
                stream_id.as_str(),
                i64::try_from(IDENTITY_BYTES_MAX)?
            ],
        )
        .await?)
}

fn current(row: &turso::Row, expected: &Recording) -> anyhow::Result<Check> {
    if row.get::<i64>(1)? == 0 {
        return Ok(Check::Catalog(Status::MissingCatalog));
    }
    if row.get::<Option<i64>>(2)? != Some(1) {
        return Ok(Check::Catalog(Status::CatalogChanged));
    }
    for (column, expected_flag, status) in [
        (6, true, Status::ActiveRecording),
        (7, false, Status::ProtectedRecording),
        (8, false, Status::CleanupPending),
    ] {
        let value = row.get::<i64>(column)?;
        anyhow::ensure!(matches!(value, 0 | 1), Failure::Invalid);
        if (value == 1) != expected_flag {
            return Ok(Check::Catalog(status));
        }
    }
    let bytes = u64::try_from(row.get::<i64>(5)?).map_err(|_| Failure::Invalid)?;
    if row.get::<i64>(3)? != expected.started_at_ms
        || row.get::<Option<i64>>(4)? != expected.ended_at_ms
        || bytes != expected.catalog_bytes
    {
        return Ok(Check::Catalog(Status::CatalogChanged));
    }
    let identity = row
        .get::<Option<String>>(10)?
        .map(|value| Identity::parse(&value).ok_or(Failure::Invalid))
        .transpose()?;
    Ok(row
        .get::<Option<String>>(9)?
        .map_or(Check::Catalog(Status::PathRejected), |path| Check::File {
            path: PathBuf::from(path),
            bytes,
            identity,
        }))
}

#[cfg(test)]
mod tests;
