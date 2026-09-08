//! Bounded catalog/filesystem drift inspection. Reports never authorize arbitrary path deletion.

use super::{
    FileIdentity, MAX_RECORDINGS, MAX_SCAN_RECORDINGS, ReadRequest, check_deadline,
    validate_identifier,
};
use crate::storage::catalog::{
    BUSY_TIMEOUT, CatalogDeletionReason, Command, RecordingCatalogHandle, SearchCommand,
    record_deletion,
};
use crate::storage::long_term::inspection::{Archive, IDENTITY_BYTES_MAX, PATH_BYTES_MAX};
use std::{collections::HashMap, fmt, io::ErrorKind, path::PathBuf, sync::mpsc, time::Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    MissingFile,
    UnknownFile,
    DuplicatePath,
    SizeMismatch,
    IdentityMismatch,
    PathRejected,
    TemporaryFile,
    InterruptedWork,
    InspectionFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
    Ignore,
    RetainTombstone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub recording_id: Option<String>,
    pub kind: Kind,
    pub label: String,
    pub bytes: Option<u64>,
}

#[derive(Clone)]
pub struct Report {
    pub id: String,
    pub revision: u64,
    pub complete: bool,
    pub items: Vec<Item>,
    pub inspected: u32,
    pub(crate) actor: String,
    pub(crate) deadline: Instant,
    candidates: HashMap<String, (Row, Kind)>,
}

impl fmt::Debug for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Report")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("complete", &self.complete)
            .field("items", &self.items)
            .field("inspected", &self.inspected)
            .finish_non_exhaustive()
    }
}

pub(in crate::storage::catalog) struct Inputs {
    revision: u64,
    rows: Vec<Row>,
    complete: bool,
}

#[derive(Clone)]
pub(in crate::storage::catalog) struct Row {
    id: String,
    path: Option<PathBuf>,
    bytes: u64,
    identity: Option<FileIdentity>,
    active: bool,
    protected: bool,
    pending: bool,
}

impl RecordingCatalogHandle {
    /// Applies an explicit remedy from an actor-owned, unexpired complete dry run.
    ///
    /// Ignoring a finding does not touch media. Retaining a tombstone removes only a still-missing,
    /// unprotected catalog row and its playback indexes; it never removes a filesystem object.
    ///
    /// # Errors
    /// Rejects stale reports, wrong owners, incomplete scans, incompatible remedies, reappeared
    /// files, changed catalog evidence, and unavailable workers.
    pub fn apply_recording_reconciliation(
        &self,
        actor: &str,
        report: &Report,
        id: &str,
        remedy: Remedy,
        archive: &Archive,
    ) -> anyhow::Result<()> {
        use super::jobs::Failure;
        anyhow::ensure!(actor == report.actor, Failure::NotFound);
        anyhow::ensure!(
            report.complete && Instant::now() < report.deadline,
            Failure::Expired
        );
        anyhow::ensure!(
            report.items.iter().any(|item| item.id == id),
            Failure::NotFound
        );
        if remedy == Remedy::Ignore {
            return Ok(());
        }
        let (row, kind) = report.candidates.get(id).ok_or(Failure::Invalid)?;
        anyhow::ensure!(
            *kind == Kind::MissingFile && !row.pending && !row.active && !row.protected,
            Failure::Blocked
        );
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let path = row.path.as_ref().ok_or(Failure::Invalid)?;
        match archive.inspect_until(path, row.bytes, deadline) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            _ => return Err(Failure::Conflict.into()),
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::ReconcileMissing {
                expected: row.clone(),
                revision: report.revision,
                deadline,
                reply,
            })
            .map_err(|_| Failure::Unavailable)?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::Unavailable)?
            .map_err(|error| {
                error
                    .downcast_ref::<Failure>()
                    .copied()
                    .unwrap_or(Failure::Unavailable)
                    .into()
            })
    }

    /// Inspects at most 4,096 catalog rows and archive entries; retains at most 128 findings.
    ///
    /// The caller must authorize Administrator access. No media or catalog row is changed.
    /// Reports explicitly mark incomplete scans. Host paths are not exposed.
    ///
    /// # Errors
    /// Rejects invalid actors, corrupt catalog rows, unavailable workers, and expired work.
    pub fn recording_reconciliation(
        &self,
        actor: &str,
        archive: &Archive,
    ) -> anyhow::Result<Report> {
        validate_identifier(actor)?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .try_send(SearchCommand::Maintenance(ReadRequest::Reconcile {
                deadline,
                reply,
            }))
            .map_err(|_| super::jobs::Failure::Unavailable)?;
        let inputs = response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| super::jobs::Failure::Unavailable)??;
        inspect(inputs, actor, archive, deadline)
    }
}

pub(in crate::storage::catalog) async fn read(
    connection: &turso::Connection,
    deadline: Instant,
) -> anyhow::Result<Inputs> {
    check_deadline(deadline)?;
    connection.execute_batch("BEGIN").await?;
    let result = read_rows(connection, deadline).await;
    match result {
        Ok(inputs) => {
            connection.execute_batch("COMMIT").await?;
            Ok(inputs)
        }
        Err(error) => {
            connection.execute_batch("ROLLBACK").await?;
            Err(error)
        }
    }
}

async fn read_rows(connection: &turso::Connection, deadline: Instant) -> anyhow::Result<Inputs> {
    let mut revisions = connection
        .query(
            "SELECT revision FROM recording_catalog_state WHERE id = 1",
            (),
        )
        .await?;
    let revision = u64::try_from(
        revisions
            .next()
            .await?
            .ok_or(super::jobs::Failure::Invalid)?
            .get::<i64>(0)?,
    )?;
    drop(revisions);
    let mut rows = connection.query(
        "SELECT id, CASE WHEN length(CAST(path AS BLOB)) <= ?2 THEN path END, file_bytes,
         CASE WHEN length(CAST(file_identity AS BLOB)) <= ?3 THEN file_identity END,
            cleanup_pending = 1 OR EXISTS
            (SELECT 1 FROM recording_maintenance_claims WHERE recording_id = recording_files.id AND active = 1)
            , finalized = 0, protected = 1
         FROM recording_files ORDER BY id LIMIT ?1",
        turso::params![i64::try_from(MAX_SCAN_RECORDINGS + 1)?, i64::try_from(PATH_BYTES_MAX)?, i64::try_from(IDENTITY_BYTES_MAX)?],
    ).await?;
    let mut inputs = Inputs {
        revision,
        rows: Vec::new(),
        complete: true,
    };
    while let Some(row) = rows.next().await? {
        check_deadline(deadline)?;
        if inputs.rows.len() == MAX_SCAN_RECORDINGS {
            inputs.complete = false;
            break;
        }
        let id: String = row.get(0)?;
        validate_identifier(&id)?;
        inputs.rows.push(Row {
            id,
            path: row.get::<Option<String>>(1)?.map(PathBuf::from),
            bytes: u64::try_from(row.get::<i64>(2)?)?,
            identity: row
                .get::<Option<String>>(3)?
                .and_then(|value| FileIdentity::parse(&value)),
            pending: row.get::<i64>(4)? != 0,
            active: row.get::<i64>(5)? != 0,
            protected: row.get::<i64>(6)? != 0,
        });
    }
    Ok(inputs)
}

fn inspect(
    inputs: Inputs,
    actor: &str,
    archive: &Archive,
    deadline: Instant,
) -> anyhow::Result<Report> {
    let mut report = Report {
        id: format!("{:032x}", rand::random::<u128>()),
        revision: inputs.revision,
        complete: inputs.complete,
        items: Vec::new(),
        inspected: 0,
        actor: actor.to_owned(),
        deadline: Instant::now() + std::time::Duration::from_secs(600),
        candidates: HashMap::new(),
    };
    let mut paths = HashMap::new();
    for row in &inputs.rows {
        if let Some(path) = &row.path {
            *paths.entry(path.clone()).or_insert(0_u32) += 1;
        }
    }
    for row in inputs.rows {
        check_deadline(deadline)?;
        report.inspected += 1;
        let kind = classify(&row, archive, &paths, deadline)?;
        if let Some(kind) = kind {
            report.push(Some(row), kind, "Catalog recording".to_owned(), None);
        }
    }
    let (files, complete) = archive.inventory(deadline)?;
    report.complete &= complete;
    for (path, temporary) in files {
        check_deadline(deadline)?;
        if paths.contains_key(&path) {
            continue;
        }
        report.inspected += 1;
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.len() <= 512)
            .unwrap_or("Unindexed file")
            .to_owned();
        report.push(
            None,
            if temporary {
                Kind::TemporaryFile
            } else {
                Kind::UnknownFile
            },
            label,
            None,
        );
    }
    check_deadline(deadline)?;
    Ok(report)
}

fn classify(
    row: &Row,
    archive: &Archive,
    paths: &HashMap<PathBuf, u32>,
    deadline: Instant,
) -> anyhow::Result<Option<Kind>> {
    let Some(path) = &row.path else {
        return Ok(Some(Kind::PathRejected));
    };
    if row.pending {
        return Ok(Some(Kind::InterruptedWork));
    }
    if row.active {
        return Ok(None);
    }
    if paths.get(path).is_some_and(|count| *count > 1) {
        return Ok(Some(Kind::DuplicatePath));
    }
    let kind = match archive.inspect_until(path, row.bytes, deadline) {
        Ok(observation)
            if row.identity == Some(FileIdentity::from_observed(observation.identity())) =>
        {
            return Ok(None);
        }
        Ok(_) => Kind::IdentityMismatch,
        Err(error) => match error.kind() {
            ErrorKind::NotFound => Kind::MissingFile,
            ErrorKind::InvalidData => Kind::SizeMismatch,
            ErrorKind::PermissionDenied => Kind::PathRejected,
            ErrorKind::TimedOut => return Err(super::jobs::Failure::Unavailable.into()),
            _ => Kind::InspectionFailed,
        },
    };
    Ok(Some(kind))
}

impl Report {
    fn push(&mut self, row: Option<Row>, kind: Kind, label: String, bytes: Option<u64>) {
        if self.items.len() == MAX_RECORDINGS {
            self.complete = false;
            return;
        }
        let id = format!("{:032x}", rand::random::<u128>());
        self.items.push(Item {
            id: id.clone(),
            recording_id: row.as_ref().map(|row| row.id.clone()),
            kind,
            label,
            bytes: row.as_ref().map(|row| row.bytes).or(bytes),
        });
        if let Some(row) = row {
            self.candidates.insert(id, (row, kind));
        }
    }
}

pub(in crate::storage::catalog) async fn remove_missing(
    connection: &turso::Connection,
    expected: &Row,
    revision: u64,
    deadline: Instant,
) -> anyhow::Result<()> {
    check_deadline(deadline)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        validate_current(connection, expected, revision).await?;
        check_deadline(deadline)?;
        record_deletion(
            connection,
            &expected.id,
            CatalogDeletionReason::Reconciliation,
        )
        .await?;
        connection
            .execute(
                "DELETE FROM recording_files WHERE id = ?1",
                turso::params![expected.id.as_str()],
            )
            .await?;
        check_deadline(deadline)?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(())
    }
    .await;
    if result.is_err() && !connection.is_autocommit()? {
        connection.execute_batch("ROLLBACK").await?;
    }
    result
}

async fn validate_current(
    connection: &turso::Connection,
    expected: &Row,
    revision: u64,
) -> anyhow::Result<()> {
    use super::jobs::Failure;
    let mut rows = connection
        .query(
            "SELECT path, file_bytes, file_identity, finalized, protected, cleanup_pending,
         (SELECT revision FROM recording_catalog_state WHERE id = 1)
         FROM recording_files WHERE id = ?1",
            turso::params![expected.id.as_str()],
        )
        .await?;
    let row = rows.next().await?.ok_or(Failure::Conflict)?;
    anyhow::ensure!(
        u64::try_from(row.get::<i64>(6)?)? == revision,
        Failure::Conflict
    );
    anyhow::ensure!(
        row.get::<i64>(3)? == 1 && row.get::<i64>(4)? == 0 && row.get::<i64>(5)? == 0,
        Failure::Blocked
    );
    let identity = row
        .get::<Option<String>>(2)?
        .and_then(|value| FileIdentity::parse(&value));
    anyhow::ensure!(
        Some(PathBuf::from(row.get::<String>(0)?)) == expected.path
            && u64::try_from(row.get::<i64>(1)?)? == expected.bytes
            && identity == expected.identity,
        Failure::Conflict
    );
    Ok(())
}
