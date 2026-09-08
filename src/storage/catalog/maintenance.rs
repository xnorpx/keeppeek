//! Read-only catalog inspection for recording-maintenance planning.
//!
//! Snapshots contain catalog observations, not authorization to delete. Callers must separately
//! check permissions, filesystem identity and confinement, and all evidence relationships before
//! proposing a destructive operation. No filesystem paths or confirmation tokens are returned.

use super::{BUSY_TIMEOUT, RecordingCatalogHandle, SearchCommand, to_u64};
use crate::storage::long_term::inspection::{IDENTITY_BYTES_MAX, Identity};
use std::{fmt, sync::mpsc, time::Instant};

pub mod evidence;
pub mod jobs;
pub mod reconciliation;

pub(super) enum ReadRequest {
    Reconcile {
        deadline: Instant,
        reply: mpsc::SyncSender<anyhow::Result<reconciliation::Inputs>>,
    },
    Jobs {
        actor: String,
        after: String,
        deadline: Instant,
        reply: mpsc::SyncSender<anyhow::Result<Vec<jobs::Job>>>,
    },
    Snapshot {
        scope: Scope,
        deadline: Instant,
        reply: mpsc::SyncSender<anyhow::Result<Snapshot>>,
    },
    Preflight {
        actor: String,
        id: String,
        deadline: Instant,
        reply: mpsc::SyncSender<anyhow::Result<jobs::preflight::Inputs>>,
    },
}

pub(super) fn read(connection: &turso::Connection, request: ReadRequest) {
    match request {
        ReadRequest::Reconcile { deadline, reply } => {
            let _ = reply.send(pollster::block_on(reconciliation::read(
                connection, deadline,
            )));
        }
        ReadRequest::Jobs {
            actor,
            after,
            deadline,
            reply,
        } => {
            let result =
                pollster::block_on(jobs::history::read(connection, &actor, &after, deadline));
            let _ = reply.send(result);
        }
        ReadRequest::Snapshot {
            scope,
            deadline,
            reply,
        } => {
            let _ = reply.send(pollster::block_on(snapshot(connection, scope, deadline)));
        }
        ReadRequest::Preflight {
            actor,
            id,
            deadline,
            reply,
        } => {
            let _ = reply.send(pollster::block_on(jobs::preflight::read(
                connection, &actor, &id, deadline,
            )));
        }
    }
}

/// Bounds retained selector and result identities independently of catalog contents.
const MAX_IDENTIFIER_BYTES: usize = 512;
/// Limits complete selections so future destructive previews remain inspectable.
const MAX_RECORDINGS: usize = 128;
/// Bounds history inspection when old long-lived objects cannot be ruled out by time alone.
const MAX_SCAN_RECORDINGS: usize = 4_096;
/// Limits requested wall-clock intervals to match existing catalog coverage windows.
const MAX_RANGE_MS: i64 = 31 * 86_400_000;

/// Selects one source and logical stream without accepting storage paths.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Scope {
    Recording {
        source_id: String,
        stream_id: String,
        recording_id: String,
    },
    /// Selects whole recording objects overlapping a half-open interval of at most 31 days.
    TimeRange {
        source_id: String,
        stream_id: String,
        start_ms: i64,
        end_ms: i64,
    },
}

impl Scope {
    fn validate(&self) -> anyhow::Result<()> {
        let (source_id, stream_id) = match self {
            Self::Recording {
                source_id,
                stream_id,
                recording_id,
            } => {
                validate_identifier(recording_id)?;
                (source_id, stream_id)
            }
            Self::TimeRange {
                source_id,
                stream_id,
                start_ms,
                end_ms,
            } => {
                let valid_range = end_ms
                    .checked_sub(*start_ms)
                    .is_some_and(|duration| (1..=MAX_RANGE_MS).contains(&duration));
                anyhow::ensure!(
                    *start_ms >= 0 && valid_range,
                    "invalid maintenance time range"
                );
                (source_id, stream_id)
            }
        };
        validate_identifier(source_id)?;
        validate_identifier(stream_id)
    }
}

/// Binds a snapshot to catalog device/file numbers without exposing the raw identifiers.
///
/// This fingerprint is not a content checksum or proof of immutable recording ownership.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct FileIdentity([u8; 32]);

impl FileIdentity {
    fn parse(value: &str) -> Option<Self> {
        Identity::parse(value).map(Self::from_observed)
    }

    pub(in crate::storage) fn from_observed(identity: Identity) -> Self {
        Self(identity.fingerprint())
    }
}

impl fmt::Debug for FileIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FileIdentity([REDACTED])")
    }
}

/// Describes the full catalog envelope of an object, including known deletion blockers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recording {
    pub recording_id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    /// The last catalog byte count, not a fresh measurement of the file.
    pub catalog_bytes: u64,
    /// Preserves the catalog identity observed at preparation; missing evidence stays unavailable.
    pub file_identity: Option<FileIdentity>,
    pub finalized: bool,
    pub protected: bool,
    pub cleanup_pending: bool,
}

/// Contains a complete, bounded selection and its consistent catalog revision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub scope: Scope,
    /// This revision covers catalog state only; it does not fence filesystem or authorization changes.
    pub revision: u64,
    pub recordings: Vec<Recording>,
    pub catalog_bytes: u64,
    pub evidence: evidence::Evidence,
}

impl RecordingCatalogHandle {
    /// Inspects at most 128 recording objects without claiming or deleting them.
    ///
    /// The caller must authorize access to the source before invoking this storage operation.
    /// Active, protected, and cleanup-pending objects remain visible. An exact ID outside the
    /// requested source/stream returns an empty selection.
    ///
    /// # Errors
    ///
    /// Rejects invalid identifiers/ranges, selections above 128 objects, a source history above
    /// 4,096 inspected objects, catalog errors, an unavailable or full search queue, and replies
    /// exceeding the catalog wait budget. An exact recording lookup avoids the history scan.
    pub fn recording_maintenance_snapshot(&self, scope: Scope) -> anyhow::Result<Snapshot> {
        scope.validate()?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .try_send(SearchCommand::Maintenance(ReadRequest::Snapshot {
                scope,
                deadline,
                reply,
            }))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => anyhow::anyhow!("recording catalog queue is full"),
                mpsc::TrySendError::Disconnected(_) => {
                    anyhow::anyhow!("recording catalog is unavailable")
                }
            })?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| {
                anyhow::anyhow!("recording catalog did not reply within its wait budget")
            })?
    }
}

pub(super) async fn snapshot(
    connection: &turso::Connection,
    scope: Scope,
    deadline: Instant,
) -> anyhow::Result<Snapshot> {
    check_deadline(deadline)?;
    connection.execute_batch("BEGIN").await?;
    let result = async {
        let snapshot = read_snapshot(connection, scope, deadline).await?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(snapshot)
    }
    .await;
    match result {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => {
            let autocommit = connection.is_autocommit().map_err(|state| {
                anyhow::anyhow!(
                    "maintenance snapshot failed: {error}; transaction state unavailable before rollback: {state}"
                )
            })?;
            if !autocommit {
                connection
                    .execute_batch("ROLLBACK")
                    .await
                    .map_err(|rollback| {
                        anyhow::anyhow!(
                            "maintenance snapshot failed: {error}; rollback failed: {rollback}"
                        )
                    })?;
            }
            Err(error)
        }
    }
}

async fn read_snapshot(
    connection: &turso::Connection,
    scope: Scope,
    deadline: Instant,
) -> anyhow::Result<Snapshot> {
    let mut revisions = connection
        .query(
            "SELECT revision FROM recording_catalog_state WHERE id = 1",
            (),
        )
        .await?;
    let revision = revisions
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording catalog revision is missing"))?;
    let revision = to_u64(revision.get(0)?, "catalog revision")?;
    drop(revisions);
    check_deadline(deadline)?;
    let rows = selected_rows(connection, &scope).await?;
    let recordings = read_recordings(rows, &scope, deadline).await?;
    let catalog_bytes = recordings.iter().try_fold(0_u64, |total, recording| {
        total
            .checked_add(recording.catalog_bytes)
            .ok_or_else(|| anyhow::anyhow!("maintenance byte count overflow"))
    })?;
    check_deadline(deadline)?;
    let evidence = evidence::read(connection, &scope, &recordings, deadline).await?;
    Ok(Snapshot {
        scope,
        revision,
        recordings,
        catalog_bytes,
        evidence,
    })
}

async fn selected_rows(
    connection: &turso::Connection,
    scope: &Scope,
) -> anyhow::Result<turso::Rows> {
    let columns = format!(
        "SELECT id, started_at_ms, ended_at_ms, file_bytes, finalized, protected, cleanup_pending,
         CASE WHEN length(CAST(file_identity AS BLOB)) > {IDENTITY_BYTES_MAX}
              THEN '' ELSE file_identity END FROM recording_files"
    );
    match scope {
        Scope::Recording {
            source_id,
            stream_id,
            recording_id,
        } => Ok(connection
            .query(
                &format!(
                    "{columns} WHERE source_id = ?1 AND logical_stream_id = ?2
                        AND id = ?3"
                ),
                turso::params![
                    source_id.as_str(),
                    stream_id.as_str(),
                    recording_id.as_str()
                ],
            )
            .await?),
        Scope::TimeRange {
            source_id,
            stream_id,
            end_ms,
            ..
        } => Ok(connection
            .query(
                &format!(
                    "{columns} INDEXED BY recording_files_source_stream_time
                        WHERE source_id = ?1 AND logical_stream_id = ?2 AND started_at_ms < ?3
                        ORDER BY started_at_ms DESC LIMIT ?4"
                ),
                turso::params![
                    source_id.as_str(),
                    stream_id.as_str(),
                    *end_ms,
                    i64::try_from(MAX_SCAN_RECORDINGS + 1)?
                ],
            )
            .await?),
    }
}

async fn read_recordings(
    mut rows: turso::Rows,
    scope: &Scope,
    deadline: Instant,
) -> anyhow::Result<Vec<Recording>> {
    let mut recordings = Vec::with_capacity(MAX_RECORDINGS);
    for scanned in 0..=MAX_SCAN_RECORDINGS {
        check_deadline(deadline)?;
        let Some(row) = rows.next().await? else {
            recordings.sort_unstable_by(|left: &Recording, right: &Recording| {
                left.started_at_ms
                    .cmp(&right.started_at_ms)
                    .then_with(|| left.recording_id.cmp(&right.recording_id))
            });
            return Ok(recordings);
        };
        anyhow::ensure!(
            scanned < MAX_SCAN_RECORDINGS,
            "maintenance history scan exceeds 4096 recordings"
        );
        let recording = read_recording(&row)?;
        if let Scope::TimeRange { start_ms, .. } = scope
            && recording.ended_at_ms.is_some_and(|end| end <= *start_ms)
        {
            continue;
        }
        anyhow::ensure!(
            recordings.len() < MAX_RECORDINGS,
            "maintenance selection exceeds 128 recordings"
        );
        recordings.push(recording);
    }
    unreachable!("history sentinel rejects the first row beyond the scan bound")
}

fn check_deadline(deadline: Instant) -> anyhow::Result<()> {
    anyhow::ensure!(
        Instant::now() < deadline,
        "maintenance snapshot deadline expired"
    );
    Ok(())
}

fn read_recording(row: &turso::Row) -> anyhow::Result<Recording> {
    let recording_id: String = row.get(0)?;
    validate_identifier(&recording_id)?;
    let started_at_ms: i64 = row.get(1)?;
    let ended_at_ms: Option<i64> = row.get(2)?;
    anyhow::ensure!(
        started_at_ms >= 0 && ended_at_ms.is_none_or(|end| end > started_at_ms),
        "invalid maintenance recording interval"
    );
    Ok(Recording {
        recording_id,
        started_at_ms,
        ended_at_ms,
        catalog_bytes: to_u64(row.get(3)?, "maintenance recording bytes")?,
        file_identity: row
            .get::<Option<String>>(7)?
            .map(|value| {
                FileIdentity::parse(&value)
                    .ok_or_else(|| anyhow::anyhow!("invalid maintenance file identity"))
            })
            .transpose()?,
        finalized: read_flag(row, 4)?,
        protected: read_flag(row, 5)?,
        cleanup_pending: read_flag(row, 6)?,
    })
}

fn read_flag(row: &turso::Row, column: usize) -> anyhow::Result<bool> {
    match row.get::<i64>(column)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => anyhow::bail!("invalid maintenance recording state"),
    }
}

fn validate_identifier(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty() && value.len() <= MAX_IDENTIFIER_BYTES && !value.contains('\0'),
        "invalid maintenance identifier"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FileIdentity, Scope, Snapshot, snapshot};
    use crate::storage::catalog::{
        CatalogRecording, Command, RecordingCatalog, RecordingCatalogHandle, SearchCommand,
        initialize_schema,
    };
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    #[test]
    fn snapshot_retains_bookmark_revision_and_reports_missing_coverage() {
        pollster::block_on(async {
            let connection = test_connection().await;
            insert_recordings(&connection, 1).await;
            connection.execute_batch(
                "INSERT INTO event_bookmarks (source_id, event_id, active, note, revision,
                 created_by, created_at_ms, updated_by, updated_at_ms, event_start_ms, event_kind)
                 VALUES ('front', 'event-selected', 1, '', 1, 'admin', 0, 'admin', 0, 500, 'motion'),
                        ('other', 'event-unrelated', 1, '', 1, 'admin', 0, 'admin', 0, 500, 'motion');"
            ).await.unwrap();
            let result = snapshot(&connection, recording_scope(), deadline())
                .await
                .unwrap();
            assert_eq!(result.evidence.bookmarks, ["event-selected"]);
            assert_eq!(result.evidence.bookmark_revision, 2);
            assert_eq!(
                result.evidence.gaps,
                [super::evidence::Interval {
                    start_ms: 0,
                    end_ms: 1_000
                }]
            );
        });
    }

    #[test]
    fn file_identity_fingerprints_are_canonical_redacted_and_bounded() {
        let identity = FileIdentity::parse("123456:987654").unwrap();
        assert_eq!(identity, FileIdentity::parse("00123456:00987654").unwrap());
        assert_ne!(identity, FileIdentity::parse("123457:987654").unwrap());
        assert_ne!(identity, FileIdentity::parse("123456:987655").unwrap());
        assert_eq!(format!("{identity:?}"), "FileIdentity([REDACTED])");
        let encoded = serde_json::to_string(&identity).unwrap();
        assert!(!encoded.contains("123456"));
        assert!(!encoded.contains("987654"));
        assert_eq!(
            serde_json::from_str::<FileIdentity>(&encoded).unwrap(),
            identity
        );
        for count in [0, 31, 33, 1_024] {
            let value = serde_json::json!(vec![0_u8; count]);
            assert!(serde_json::from_value::<FileIdentity>(value).is_err());
        }
        let invalid_byte = serde_json::json!(vec![256; 32]);
        assert!(serde_json::from_value::<FileIdentity>(invalid_byte).is_err());
    }

    #[test]
    fn snapshot_rejects_malformed_identity_without_leaking_values_or_retaining_a_transaction() {
        pollster::block_on(async {
            let connection = test_connection().await;
            insert_recordings(&connection, 1).await;
            for invalid in [
                "private-identity",
                "1:2:3",
                &"private-identity".repeat(100_000),
            ] {
                connection
                    .execute(
                        "UPDATE recording_files SET file_identity = ?1",
                        turso::params![invalid],
                    )
                    .await
                    .unwrap();
                let error = snapshot(&connection, recording_scope(), deadline())
                    .await
                    .unwrap_err();
                assert!(!format!("{error:?}").contains("private-identity"));
                assert!(connection.is_autocommit().unwrap());
            }
            connection
                .execute("UPDATE recording_files SET file_identity = '1:2'", ())
                .await
                .unwrap();
            let snapshot = snapshot(&connection, recording_scope(), deadline())
                .await
                .unwrap();
            assert_eq!(
                snapshot.recordings[0].file_identity,
                FileIdentity::parse("1:2")
            );
        });
    }

    #[test]
    fn exact_snapshot_reports_protection_without_claiming_or_removing_media() {
        let root =
            std::env::temp_dir().join(format!("keeppeek-maintenance-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&root).unwrap();
        let recording_path = root.join("recording.mp4");
        let bytes = vec![42; 64];
        std::fs::write(&recording_path, &bytes).unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front/sub".to_owned(),
                source_id: Some("front".to_owned()),
                logical_stream_id: Some("sub".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: recording_path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
        handle
            .update_recording_path("recording-1", &recording_path, true)
            .unwrap();
        handle.set_recording_protected("recording-1", true).unwrap();
        let before = handle.stats().unwrap();

        let snapshot = handle
            .recording_maintenance_snapshot(Scope::Recording {
                source_id: "front".to_owned(),
                stream_id: "sub".to_owned(),
                recording_id: "recording-1".to_owned(),
            })
            .unwrap();

        assert_eq!(snapshot.recordings.len(), 1);
        assert_eq!(snapshot.recordings[0].recording_id, "recording-1");
        assert!(snapshot.recordings[0].protected);
        assert!(!snapshot.recordings[0].cleanup_pending);
        assert_eq!(snapshot.catalog_bytes, 64);
        assert_eq!(handle.stats().unwrap(), before);
        assert!(handle.pending_cleanup_candidate().unwrap().is_none());
        assert_eq!(std::fs::read(&recording_path).unwrap(), bytes);

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_scope_cannot_cross_source_or_logical_stream_boundaries() {
        pollster::block_on(async {
            let connection = test_connection().await;
            insert_recordings(&connection, 1).await;
            for scope in [
                exact_scope("other", "sub", "recording-00000"),
                exact_scope("front", "main", "recording-00000"),
                exact_scope("front", "sub", "' OR 1=1 --"),
            ] {
                let result = snapshot(&connection, scope, deadline()).await.unwrap();
                assert!(result.recordings.is_empty());
                assert_eq!(result.catalog_bytes, 0);
            }
            let result = snapshot(&connection, recording_scope(), deadline())
                .await
                .unwrap();
            assert_eq!(result.recordings[0].recording_id, "recording-00000");
            assert_eq!(result.catalog_bytes, 64);
        });
    }

    #[test]
    fn half_open_range_preserves_full_object_bounds_and_blocked_states() {
        pollster::block_on(async {
            let connection = test_connection().await;
            insert_recordings(&connection, 7).await;
            connection
                .execute_batch(
                    "UPDATE recording_files SET ended_at_ms = 2500, cleanup_pending = 1
                    WHERE id = 'recording-00000';
                 UPDATE recording_files SET ended_at_ms = 3500 WHERE id = 'recording-00001';
                 UPDATE recording_files SET protected = 1 WHERE id = 'recording-00002';
                 UPDATE recording_files SET started_at_ms = 2500, ended_at_ms = NULL, finalized = 0
                    WHERE id = 'recording-00005';
                 UPDATE recording_files SET started_at_ms = 0, ended_at_ms = 2000
                    WHERE id = 'recording-00006';",
                )
                .await
                .unwrap();
            let result = snapshot(&connection, range_scope(2_000, 3_000), deadline())
                .await
                .unwrap();
            assert_eq!(
                result
                    .recordings
                    .iter()
                    .map(|row| row.recording_id.as_str())
                    .collect::<Vec<_>>(),
                [
                    "recording-00000",
                    "recording-00001",
                    "recording-00002",
                    "recording-00005"
                ]
            );
            assert_eq!(result.recordings[0].started_at_ms, 0);
            assert_eq!(result.recordings[1].ended_at_ms, Some(3_500));
            assert!(result.recordings[0].cleanup_pending);
            assert!(result.recordings[2].protected);
            assert!(!result.recordings[3].finalized);
            assert_eq!(result.recordings[3].ended_at_ms, None);
            assert_eq!(result.catalog_bytes, 256);
        });
    }

    #[test]
    fn oversized_selection_is_rejected_without_a_partial_snapshot_or_open_transaction() {
        pollster::block_on(async {
            let connection = test_connection().await;
            insert_recordings(&connection, 129).await;
            let accepted = snapshot(&connection, range_scope(0, 128_000), deadline())
                .await
                .unwrap();
            assert_eq!(accepted.recordings.len(), 128);
            assert_eq!(accepted.catalog_bytes, 8_192);
            let error = snapshot(&connection, range_scope(0, 129_000), deadline())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("exceeds 128"));
            assert!(connection.is_autocommit().unwrap());
            let after = snapshot(&connection, recording_scope(), deadline())
                .await
                .unwrap();
            assert_eq!(after.recordings.len(), 1);
            assert_eq!(after.revision, accepted.revision);
        });
    }

    #[test]
    fn history_scan_limit_does_not_mistake_an_incomplete_scan_for_an_empty_range() {
        pollster::block_on(async {
            let connection = test_connection().await;
            insert_recordings(&connection, 4_097).await;
            let within = snapshot(&connection, range_scope(4_095_999, 4_096_000), deadline())
                .await
                .unwrap();
            assert_eq!(within.recordings.len(), 1);
            let error = snapshot(&connection, range_scope(4_098_000, 4_099_000), deadline())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("history scan exceeds 4096"));
            assert!(connection.is_autocommit().unwrap());
            let exact = snapshot(&connection, recording_scope(), deadline())
                .await
                .unwrap();
            assert_eq!(exact.recordings.len(), 1);
        });
    }

    #[test]
    fn expired_work_is_rejected_before_opening_a_database_transaction() {
        pollster::block_on(async {
            let connection = test_connection().await;
            connection.execute_batch("BEGIN").await.unwrap();
            let error = snapshot(&connection, recording_scope(), Instant::now())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("deadline expired"));
            assert!(!connection.is_autocommit().unwrap());
            connection.execute_batch("ROLLBACK").await.unwrap();
        });
    }

    #[test]
    fn invalid_scopes_are_rejected_before_admission_to_the_read_worker() {
        let (write_tx, _) = mpsc::sync_channel(1);
        let (search_tx, _) = mpsc::sync_channel(1);
        let handle = RecordingCatalogHandle {
            tx: write_tx,
            search_tx,
        };
        for scope in [
            range_scope(0, 0),
            range_scope(10, 9),
            range_scope(-1, 1),
            range_scope(0, 31 * 86_400_000 + 1),
            range_scope(0, i64::MAX),
            exact_scope("", "sub", "recording-00000"),
            exact_scope("front", "\0", "recording-00000"),
            exact_scope("front", "sub", " "),
            exact_scope("front", "sub", &"x".repeat(513)),
        ] {
            let error = handle.recording_maintenance_snapshot(scope).unwrap_err();
            assert!(error.to_string().contains("invalid maintenance"));
        }
    }

    #[test]
    fn inspection_does_not_enqueue_on_a_blocked_writer() {
        let (write_tx, write_rx) = mpsc::sync_channel(1);
        write_tx.send(Command::Shutdown).unwrap();
        let (search_tx, search_rx) = mpsc::sync_channel(1);
        let handle = RecordingCatalogHandle {
            tx: write_tx,
            search_tx,
        };
        let worker = std::thread::spawn(move || {
            let SearchCommand::Maintenance(super::ReadRequest::Snapshot {
                scope,
                deadline,
                reply,
            }) = search_rx.recv_timeout(Duration::from_secs(1)).unwrap()
            else {
                panic!("inspection must reach the search worker");
            };
            assert!(deadline > Instant::now());
            reply
                .send(Ok(Snapshot {
                    scope,
                    revision: 42,
                    recordings: Vec::new(),
                    catalog_bytes: 0,
                    evidence: super::evidence::Evidence {
                        bookmark_revision: 0,
                        bookmarks: Vec::new(),
                        gaps: Vec::new(),
                    },
                }))
                .unwrap();
        });
        let result = handle
            .recording_maintenance_snapshot(recording_scope())
            .unwrap();
        worker.join().unwrap();
        assert_eq!(result.revision, 42);
        assert!(matches!(write_rx.try_recv(), Ok(Command::Shutdown)));
    }

    #[test]
    fn full_and_disconnected_read_queues_fail_without_waiting_for_writer_capacity() {
        let (write_tx, _) = mpsc::sync_channel(1);
        let (search_tx, search_rx) = mpsc::sync_channel(1);
        search_tx.send(SearchCommand::Shutdown).unwrap();
        let handle = RecordingCatalogHandle {
            tx: write_tx,
            search_tx,
        };
        let full = handle
            .recording_maintenance_snapshot(recording_scope())
            .unwrap_err();
        assert!(full.to_string().contains("queue is full"));
        drop(search_rx);
        let disconnected = handle
            .recording_maintenance_snapshot(recording_scope())
            .unwrap_err();
        assert!(disconnected.to_string().contains("unavailable"));
    }

    #[test]
    fn snapshot_revision_and_rows_share_one_read_transaction_while_writes_continue() {
        pollster::block_on(async {
            let database = turso::Builder::new_local(":memory:").build().await.unwrap();
            let writer = database.connect().unwrap();
            let reader = database.connect().unwrap();
            writer.busy_timeout(Duration::from_millis(500)).unwrap();
            initialize_schema(&writer).await.unwrap();
            insert_recordings(&writer, 1).await;
            let original = snapshot(&reader, recording_scope(), deadline())
                .await
                .unwrap();
            reader.execute_batch("BEGIN").await.unwrap();
            let mut revisions = reader
                .query("SELECT revision FROM recording_catalog_state", ())
                .await
                .unwrap();
            assert_eq!(
                revisions
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                i64::try_from(original.revision).unwrap()
            );
            drop(revisions);
            writer
                .execute("UPDATE recording_files SET protected = 1", ())
                .await
                .unwrap();

            let consistent = super::read_snapshot(&reader, recording_scope(), deadline())
                .await
                .unwrap();
            assert_eq!(consistent, original);
            reader.execute_batch("COMMIT").await.unwrap();
            let latest = snapshot(&reader, recording_scope(), deadline())
                .await
                .unwrap();
            assert!(latest.recordings[0].protected);
            assert!(latest.revision > original.revision);
        });
    }

    #[test]
    fn corrupt_catalog_state_fails_closed_and_preserves_read_connection_reuse() {
        pollster::block_on(async {
            let connection = test_connection().await;
            insert_recordings(&connection, 1).await;
            for corruption in [
                "UPDATE recording_files SET file_bytes = -1",
                "UPDATE recording_files SET started_at_ms = -1",
                "UPDATE recording_files SET ended_at_ms = 0",
                "UPDATE recording_files SET protected = 2",
                "UPDATE recording_files SET finalized = -1",
                "UPDATE recording_files SET cleanup_pending = 2",
            ] {
                connection.execute_batch(corruption).await.unwrap();
                assert!(
                    snapshot(&connection, recording_scope(), deadline())
                        .await
                        .is_err()
                );
                assert!(connection.is_autocommit().unwrap());
                connection
                    .execute_batch(
                        "UPDATE recording_files SET file_bytes = 64, started_at_ms = 0,
                        ended_at_ms = 1000, protected = 0, finalized = 1, cleanup_pending = 0",
                    )
                    .await
                    .unwrap();
            }
            let recovered = snapshot(&connection, recording_scope(), deadline())
                .await
                .unwrap();
            assert_eq!(recovered.catalog_bytes, 64);
        });
    }

    async fn test_connection() -> turso::Connection {
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let connection = database.connect().unwrap();
        initialize_schema(&connection).await.unwrap();
        connection
    }

    async fn insert_recordings(connection: &turso::Connection, count: i64) {
        assert!((1..=4_097).contains(&count));
        connection.execute_batch("BEGIN IMMEDIATE").await.unwrap();
        for ordinal in 0..count {
            connection
                .execute(
                    "INSERT INTO recording_files
                     (id, stream_id, source_id, logical_stream_id, started_at_ms, ended_at_ms,
                      path, init_offset, init_len, finalized, file_bytes)
                 VALUES (?1, 'front/sub', 'front', 'sub', ?2, ?3, ?4, 0, 8, 1, 64)",
                    turso::params![
                        format!("recording-{ordinal:05}"),
                        ordinal * 1_000,
                        (ordinal + 1) * 1_000,
                        format!("synthetic-{ordinal:05}.mp4"),
                    ],
                )
                .await
                .unwrap();
        }
        connection.execute_batch("COMMIT").await.unwrap();
    }

    fn recording_scope() -> Scope {
        exact_scope("front", "sub", "recording-00000")
    }

    fn exact_scope(source_id: &str, stream_id: &str, recording_id: &str) -> Scope {
        Scope::Recording {
            source_id: source_id.to_owned(),
            stream_id: stream_id.to_owned(),
            recording_id: recording_id.to_owned(),
        }
    }

    fn range_scope(start_ms: i64, end_ms: i64) -> Scope {
        Scope::TimeRange {
            source_id: "front".to_owned(),
            stream_id: "sub".to_owned(),
            start_ms,
            end_ms,
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }
}
