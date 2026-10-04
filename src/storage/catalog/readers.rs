//! Reader leases bind a catalog resolution to the lifetime of its file reader.

use super::{CatalogMediaFragment, CatalogMediaObjectLocation, Command, RecordingCatalogHandle};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak, mpsc},
};

const MAX_LOCATIONS: usize = 4096;
const MAX_MOVE_WORKERS: usize = 4096;
type Key = (String, String);

#[derive(Default)]
pub struct Registry {
    active: Mutex<BTreeMap<Key, usize>>,
    moves: Mutex<BTreeMap<String, Weak<()>>>,
    authority: Weak<super::authority::Lease>,
}

impl Registry {
    pub(super) fn new(authority: &Arc<super::authority::Lease>) -> Self {
        Self {
            active: Mutex::new(BTreeMap::new()),
            moves: Mutex::new(BTreeMap::new()),
            authority: Arc::downgrade(authority),
        }
    }

    fn claim_move(&self, job_id: &str) -> anyhow::Result<MoveLease> {
        anyhow::ensure!(
            !job_id.is_empty() && job_id.len() <= 256 && !job_id.chars().any(char::is_control),
            "invalid volume move identity"
        );
        let authority = self
            .authority
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("catalog authority is unavailable"))?;
        let mut moves = self
            .moves
            .lock()
            .map_err(|_| anyhow::anyhow!("volume move registry is poisoned"))?;
        // ponytail: Reap at most 4096 expired workers on admission; no cleanup thread.
        moves.retain(|_, worker| worker.strong_count() != 0);
        anyhow::ensure!(
            !moves.contains_key(job_id),
            "volume move worker is already active"
        );
        anyhow::ensure!(
            moves.len() < MAX_MOVE_WORKERS,
            "volume move worker limit exceeded"
        );
        let token = Arc::new(());
        moves.insert(job_id.to_owned(), Arc::downgrade(&token));
        Ok(MoveLease {
            _token: token,
            _authority: authority,
        })
    }

    /// Fail closed when a reader holds this object or its destination path.
    pub(crate) fn conflicts(&self, recording_id: &str, path: &str) -> anyhow::Result<bool> {
        let active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("recording reader registry is poisoned"))?;
        // ponytail: scan at most 4096 entries; add indexes only if admission becomes hot.
        Ok(active.keys().any(|(id, location)| {
            id == recording_id
                || location
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(&path.replace('\\', "/"))
        }))
    }

    fn acquire(self: &Arc<Self>, keys: BTreeSet<Key>) -> anyhow::Result<LeaseSet> {
        anyhow::ensure!(
            keys.len() <= MAX_LOCATIONS,
            "recording reader location limit exceeded"
        );
        let mut active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("recording reader registry is poisoned"))?;
        let additional = keys.iter().filter(|key| !active.contains_key(*key)).count();
        anyhow::ensure!(
            active.len() + additional <= MAX_LOCATIONS,
            "recording reader location limit exceeded"
        );
        anyhow::ensure!(
            keys.iter()
                .all(|key| active.get(key).is_none_or(|count| *count < usize::MAX)),
            "recording reader count overflow"
        );
        for key in &keys {
            *active.entry(key.clone()).or_default() += 1;
        }
        Ok(LeaseSet {
            _authority: if keys.is_empty() {
                None
            } else {
                self.authority.upgrade()
            },
            registry: Arc::clone(self),
            keys,
        })
    }
}

/// Keep this guard in the actual move worker through file and catalog publication.
#[must_use]
pub struct MoveLease {
    _token: Arc<()>,
    _authority: Arc<super::authority::Lease>,
}

/// Keep this guard in the worker until all source file handles have closed.
#[must_use]
pub struct LeaseSet {
    _authority: Option<Arc<super::authority::Lease>>,
    registry: Arc<Registry>,
    keys: BTreeSet<Key>,
}

impl Drop for LeaseSet {
    fn drop(&mut self) {
        let Ok(mut active) = self.registry.active.lock() else {
            // A poisoned registry retains its entries and rejects future mutations.
            return;
        };
        for key in &self.keys {
            if let Some(count) = active.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    active.remove(key);
                }
            }
        }
    }
}

pub(super) enum Request {
    Fragments {
        stream: String,
        start: i64,
        end: i64,
    },
    Object {
        source: String,
        stream: String,
        legacy: Option<String>,
        id: String,
        sequence: u64,
    },
    Snapshots(BTreeSet<Key>),
    Export(String),
    Image {
        event: String,
        attachment: String,
        revision: u64,
    },
}

pub(super) enum Reply {
    Fragments(Vec<CatalogMediaFragment>, LeaseSet),
    Object(Option<(CatalogMediaObjectLocation, LeaseSet)>),
    Snapshots(LeaseSet),
    Image(Option<(super::locations::Location, LeaseSet)>),
    Export(Option<(super::locations::Location, LeaseSet)>),
}

impl RecordingCatalogHandle {
    /// Resolves an export and prevents its retirement until the returned lease closes.
    ///
    /// # Errors
    /// Rejects retiring exports and unavailable catalog authority.
    pub fn leased_export(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<(super::locations::Location, LeaseSet)>> {
        match self.read_lease(Request::Export(id.into()))? {
            Reply::Export(export) => Ok(export),
            _ => anyhow::bail!("unexpected export reader reply"),
        }
    }

    pub(crate) fn leased_event_image(
        &self,
        event: &super::TimelineEvent,
        attachment: &str,
    ) -> anyhow::Result<Option<(super::locations::Location, LeaseSet)>> {
        match self.read_lease(Request::Image {
            event: event.id.clone(),
            attachment: attachment.to_owned(),
            revision: event.revision,
        })? {
            Reply::Image(image) => Ok(image),
            _ => anyhow::bail!("unexpected event image reader reply"),
        }
    }

    pub(crate) fn claim_volume_move(&self, job_id: &str) -> anyhow::Result<MoveLease> {
        self.readers.claim_move(job_id)
    }

    pub(crate) const fn reader_leases(&self) -> &Arc<Registry> {
        &self.readers
    }

    pub(crate) fn leased_media_fragments_in_range(
        &self,
        stream: &str,
        start: i64,
        end: i64,
    ) -> anyhow::Result<(Vec<CatalogMediaFragment>, LeaseSet)> {
        anyhow::ensure!(start < end, "recording range must be nonempty");
        match self.read_lease(Request::Fragments {
            stream: stream.to_owned(),
            start,
            end,
        })? {
            Reply::Fragments(fragments, lease) => Ok((fragments, lease)),
            _ => anyhow::bail!("unexpected recording reader reply"),
        }
    }

    pub(crate) fn leased_resolve_media_object(
        &self,
        source: &str,
        stream: &str,
        legacy: Option<&str>,
        id: &str,
        sequence: u64,
    ) -> anyhow::Result<Option<(CatalogMediaObjectLocation, LeaseSet)>> {
        match self.read_lease(Request::Object {
            source: source.to_owned(),
            stream: stream.to_owned(),
            legacy: legacy.map(str::to_owned),
            id: id.to_owned(),
            sequence,
        })? {
            Reply::Object(object) => Ok(object),
            _ => anyhow::bail!("unexpected recording reader reply"),
        }
    }

    pub(crate) fn lease_media_fragments(
        &self,
        fragments: &[CatalogMediaFragment],
    ) -> anyhow::Result<LeaseSet> {
        let keys = fragment_keys(fragments)?;
        match self.read_lease(Request::Snapshots(keys))? {
            Reply::Snapshots(lease) => Ok(lease),
            _ => anyhow::bail!("unexpected recording reader reply"),
        }
    }

    pub(crate) fn lease_event_keyframe(
        &self,
        location: &super::EventKeyframeLocation,
    ) -> anyhow::Result<LeaseSet> {
        let keys = BTreeSet::from([(location.recording_id.clone(), location.path.clone())]);
        match self.read_lease(Request::Snapshots(keys))? {
            Reply::Snapshots(lease) => Ok(lease),
            _ => anyhow::bail!("unexpected recording reader reply"),
        }
    }

    fn read_lease(&self, request: Request) -> anyhow::Result<Reply> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::ReadLease { request, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv_timeout(super::BUSY_TIMEOUT)
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }
}

fn fragment_keys(fragments: &[CatalogMediaFragment]) -> anyhow::Result<BTreeSet<Key>> {
    let mut keys = BTreeSet::new();
    for fragment in fragments {
        keys.insert((fragment.recording_id.clone(), fragment.path.clone()));
        anyhow::ensure!(
            keys.len() <= MAX_LOCATIONS,
            "recording reader location limit exceeded"
        );
    }
    Ok(keys)
}

pub(super) async fn execute(
    connection: &turso::Connection,
    registry: &Arc<Registry>,
    request: Request,
) -> anyhow::Result<Reply> {
    match request {
        Request::Image {
            event,
            attachment,
            revision,
        } => image(connection, registry, &event, &attachment, revision).await,
        Request::Fragments { stream, start, end } => {
            let fragments =
                super::media_fragments_in_range(connection, &stream, start, end).await?;
            let keys = fragment_keys(&fragments)?;
            let keys = owned_locations(connection, keys).await?;
            Ok(Reply::Fragments(fragments, registry.acquire(keys)?))
        }
        Request::Object {
            source,
            stream,
            legacy,
            id,
            sequence,
        } => {
            let object = super::resolve_media_object(
                connection,
                &source,
                &stream,
                legacy.as_deref(),
                &id,
                sequence,
            )
            .await?;
            let Some(object) = object else {
                return Ok(Reply::Object(None));
            };
            let keys = BTreeSet::from([(object.recording_id.clone(), object.path.clone())]);
            let keys = owned_locations(connection, keys).await?;
            Ok(Reply::Object(Some((object, registry.acquire(keys)?))))
        }
        Request::Snapshots(keys) => {
            let keys = owned_locations(connection, keys).await?;
            Ok(Reply::Snapshots(registry.acquire(keys)?))
        }
        Request::Export(id) => {
            let Some(location) =
                super::locations::export_cleanup::readable(connection, &id).await?
            else {
                return Ok(Reply::Export(None));
            };
            let lease = volume_lease(connection, registry, &location).await?;
            Ok(Reply::Export(Some((location, lease))))
        }
    }
}

async fn image(
    connection: &turso::Connection,
    registry: &Arc<Registry>,
    event: &str,
    attachment: &str,
    revision: u64,
) -> anyhow::Result<Reply> {
    let current = super::event_by_id(connection, event)
        .await?
        .ok_or_else(|| anyhow::anyhow!("event image owner disappeared"))?;
    anyhow::ensure!(current.revision == revision, "event image owner changed");
    anyhow::ensure!(
        current.attachments.iter().any(|item| item.id == attachment),
        "event attachment disappeared"
    );
    let Some(location) = super::locations::images::lookup(connection, event, attachment).await?
    else {
        return Ok(Reply::Image(None));
    };
    let lease = volume_lease(connection, registry, &location).await?;
    Ok(Reply::Image(Some((location, lease))))
}

async fn volume_lease(
    connection: &turso::Connection,
    registry: &Arc<Registry>,
    location: &super::locations::Location,
) -> anyhow::Result<LeaseSet> {
    let mut rows = connection
        .query(
            "SELECT root FROM storage_volume_bindings WHERE id=?1 AND generation=?2",
            turso::params![
                location.volume.clone(),
                super::to_i64(location.generation, "volume generation")?
            ],
        )
        .await?;
    let root = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("media volume binding disappeared"))?
        .get::<String>(0)?;
    let path = std::path::Path::new(&root).join(&location.relative_key);
    let keys = BTreeSet::from([(
        location.object.id.clone(),
        path.to_string_lossy().into_owned(),
    )]);
    registry.acquire(keys)
}

async fn owned_locations(
    connection: &turso::Connection,
    keys: BTreeSet<Key>,
) -> anyhow::Result<BTreeSet<Key>> {
    anyhow::ensure!(
        keys.len() <= MAX_LOCATIONS,
        "recording reader location limit exceeded"
    );
    let mut owned = BTreeSet::new();
    for (id, path) in keys {
        let mut rows = connection.query(
            "SELECT EXISTS (SELECT 1 FROM storage_volume_allocations a WHERE a.kind = 'recording' AND a.state != 'cancelled' AND (a.object_id = r.id OR a.destination_path = replace(r.path, char(92), '/') COLLATE NOCASE))
             FROM recording_files r WHERE r.id = ?1 AND r.path = ?2 AND r.cleanup_pending = 0
             AND NOT EXISTS (SELECT 1 FROM storage_volume_moves m JOIN storage_volume_allocations source ON source.operation = m.source_operation
                 WHERE m.phase IN ('published','retiring','complete') AND source.destination_path = replace(r.path, char(92), '/') COLLATE NOCASE)
             AND NOT EXISTS (SELECT 1 FROM recording_maintenance_claims WHERE (recording_id = r.id OR replace(path, char(92), '/') = replace(r.path, char(92), '/') COLLATE NOCASE) AND active = 1)",
            (id.as_str(), path.as_str()),
        ).await?;
        let row = rows.next().await?.ok_or_else(|| {
            anyhow::anyhow!("recording reader location changed or is unavailable")
        })?;
        // Legacy archive rotation precedes its catalog path update. It cannot use
        // this fence until its owner adopts the location transition protocol.
        if row.get::<i64>(0)? != 0 {
            owned.insert((id, path));
        }
    }
    Ok(owned)
}

pub(super) async fn ensure_recording_idle(
    connection: &turso::Connection,
    registry: &Registry,
    id: &str,
    destination: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !registry.conflicts(id, destination)?,
        "recording has active readers"
    );
    let mut rows = connection
        .query("SELECT path FROM recording_files WHERE id = ?1", [id])
        .await?;
    if let Some(row) = rows.next().await? {
        let path: String = row.get(0)?;
        anyhow::ensure!(
            !registry.conflicts(id, &path)?,
            "recording has active readers"
        );
    }
    Ok(())
}

pub(super) async fn ensure_cleanup_idle(
    connection: &turso::Connection,
    registry: &Registry,
) -> anyhow::Result<()> {
    let mut rows = connection.query(
        "SELECT id, path FROM recording_files WHERE finalized = 1 AND protected = 0
         AND NOT EXISTS (SELECT 1 FROM recording_maintenance_claims WHERE recording_id = recording_files.id AND active = 1)
         ORDER BY cleanup_pending DESC, started_at_ms, id LIMIT 1", ()).await?;
    if let Some(row) = rows.next().await? {
        anyhow::ensure!(
            !registry.conflicts(&row.get::<String>(0)?, &row.get::<String>(1)?)?,
            "cleanup candidate has active readers"
        );
    }
    Ok(())
}

pub(super) async fn ensure_job_idle(
    connection: &turso::Connection,
    registry: &Registry,
    actor: &str,
    id: &str,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT snapshot_json FROM recording_maintenance_intents WHERE id = ?1 AND actor = ?2",
            (id, actor),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(());
    };
    let snapshot: super::maintenance::Snapshot = serde_json::from_str(&row.get::<String>(0)?)?;
    anyhow::ensure!(
        snapshot.recordings.len() <= super::maintenance::MAX_RECORDINGS,
        "maintenance reader check limit exceeded"
    );
    drop(rows);
    for recording in snapshot.recordings {
        ensure_recording_idle(connection, registry, &recording.recording_id, "").await?;
    }
    Ok(())
}

pub(super) fn ensure_idle(registry: &Registry) -> anyhow::Result<()> {
    let active = registry
        .active
        .lock()
        .map_err(|_| anyhow::anyhow!("recording reader registry is poisoned"))?;
    // ponytail: rare reconciliation requires all readers idle; narrow this if needed.
    anyhow::ensure!(active.is_empty(), "recording readers are active");
    Ok(())
}

pub(super) async fn ensure_path_change_idle(
    connection: &turso::Connection,
    registry: &Registry,
    id: &str,
    destination: &str,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query("SELECT path FROM recording_files WHERE id = ?1", [id])
        .await?;
    if let Some(row) = rows.next().await? {
        let old: String = row.get(0)?;
        if old == destination {
            return Ok(());
        }
    }
    drop(rows);
    ensure_recording_idle(connection, registry, id, destination).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::catalog::{
        CatalogFragment, CatalogKeyframe, CatalogRecording, RecordingCatalog,
    };

    fn named_path(root: &std::path::Path, handle: &RecordingCatalogHandle) -> String {
        use super::super::locations::{
            Allocation, Binding, Capacity, Kind, Object, Reply, Request,
        };
        handle
            .volume_location(Request::Bind(Binding {
                id: "readers".into(),
                generation: 1,
                root: root.to_path_buf(),
                filesystem: "readers-fs".into(),
                root_identity: "readers-root".into(),
                writable: true,
                draining: false,
                limit_bytes: Some(4096),
                minimum_free_bytes: 0,
            }))
            .unwrap();
        let Reply::Revision(revision) = handle.volume_location(Request::Revision).unwrap() else {
            panic!("revision missing")
        };
        handle
            .volume_location(Request::Reserve(Allocation {
                operation: "reader-allocation".into(),
                object: Object {
                    kind: Kind::Recording,
                    id: "reader-recording".into(),
                },
                volume: "readers".into(),
                generation: 1,
                relative_key: "reader.mp4".into(),
                bytes: 120,
                capacity: Capacity {
                    ledger_revision: revision,
                    observed_at: std::time::Instant::now(),
                    available_bytes: 4096,
                    filesystem: "readers-fs".into(),
                    root_identity: "readers-root".into(),
                },
            }))
            .unwrap();
        root.join("reader.mp4").to_string_lossy().into_owned()
    }

    fn fixture(
        name: &str,
        named: bool,
    ) -> (std::path::PathBuf, RecordingCatalog, RecordingCatalogHandle) {
        let root = super::super::tests::test_dir(name);
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let path = if named {
            named_path(&root, &handle)
        } else {
            "reader.mp4".into()
        };
        handle
            .upsert_recording(CatalogRecording {
                id: "reader-recording".into(),
                stream_id: "camera/main".into(),
                source_id: Some("camera".into()),
                logical_stream_id: Some("main".into()),
                started_at_ms: 1000,
                ended_at_ms: Some(3000),
                path,
                init_offset: 0,
                init_len: 20,
                finalized: true,
            })
            .unwrap();
        handle
            .insert_fragment_with_keyframe(
                CatalogFragment {
                    recording_id: "reader-recording".into(),
                    sequence: 1,
                    start_ms: 1000,
                    duration_ms: 2000,
                    byte_offset: 20,
                    byte_len: 100,
                    random_access: true,
                },
                CatalogKeyframe {
                    recording_id: "reader-recording".into(),
                    fragment_sequence: 1,
                    byte_offset: 40,
                    byte_len: 30,
                },
            )
            .unwrap();
        (root, catalog, handle)
    }

    #[test]
    fn shared_reader_leases_release_only_after_last_worker() {
        let registry = Arc::new(Registry::default());
        let keys = BTreeSet::from([("one".into(), "root/file.mp4".into())]);
        let first = registry.acquire(keys.clone()).unwrap();
        let second = registry.acquire(keys).unwrap();
        assert!(registry.conflicts("other", "ROOT\\file.mp4").unwrap());
        drop(first);
        assert!(registry.conflicts("one", "different").unwrap());
        let (ready, started) = mpsc::sync_channel(1);
        let (release, finish) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _lease = second;
            ready.send(()).unwrap();
            finish
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        });
        started
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(registry.conflicts("one", "").unwrap());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(!registry.conflicts("one", "").unwrap());
    }

    #[test]
    fn reader_capacity_rejection_preserves_existing_counts() {
        let registry = Arc::new(Registry::default());
        let keys = (0..MAX_LOCATIONS)
            .map(|n| (n.to_string(), format!("{n}.mp4")))
            .collect();
        let full = registry.acquire(keys).unwrap();
        let shared = registry
            .acquire(BTreeSet::from([("0".into(), "0.mp4".into())]))
            .unwrap();
        assert!(
            registry
                .acquire(BTreeSet::from([
                    ("0".into(), "0.mp4".into()),
                    ("overflow".into(), "overflow.mp4".into())
                ]))
                .is_err()
        );
        assert_eq!(
            *registry
                .active
                .lock()
                .unwrap()
                .get(&("0".into(), "0.mp4".into()))
                .unwrap(),
            2
        );
        drop(full);
        assert!(registry.conflicts("0", "").unwrap());
        drop(shared);
        assert!(registry.active.lock().unwrap().is_empty());
    }

    #[test]
    fn named_resolution_retains_both_reader_leases_until_drop() {
        let (root, catalog, handle) = fixture("reader-leases-fences", true);
        let (fragments, range_lease) = handle
            .leased_media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        let (_, object_lease) = handle
            .leased_resolve_media_object("camera", "main", None, "reader-recording", 1)
            .unwrap()
            .unwrap();
        assert_eq!(fragments.len(), 1);
        assert!(handle.claim_cleanup_candidate().is_err());
        assert!(handle.delete_recording("reader-recording").is_err());
        assert!(
            handle
                .update_recording_path("reader-recording", std::path::Path::new("moved.mp4"), true)
                .is_err()
        );
        drop(range_lease);
        assert!(
            handle
                .reader_leases()
                .conflicts("reader-recording", "")
                .unwrap()
        );
        drop(object_lease);
        let second = handle.lease_media_fragments(&fragments).unwrap();
        assert!(
            handle
                .reader_leases()
                .conflicts("reader-recording", "")
                .unwrap()
        );
        drop(second);
        assert!(
            !handle
                .reader_leases()
                .conflicts("reader-recording", "")
                .unwrap()
        );
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn export_snapshot_rejects_path_change_without_leaking_a_lease() {
        let (root, catalog, handle) = fixture("reader-leases-snapshot", false);
        let (fragments, legacy_guard) = handle
            .leased_media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        assert!(
            !handle
                .reader_leases()
                .conflicts("reader-recording", "")
                .unwrap()
        );
        handle
            .update_recording_path("reader-recording", std::path::Path::new("moved.mp4"), true)
            .unwrap();
        assert!(handle.lease_media_fragments(&fragments).is_err());
        assert!(
            !handle
                .reader_leases()
                .conflicts("reader-recording", "")
                .unwrap()
        );
        let (fresh, lease) = handle
            .leased_media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        assert_eq!(fresh[0].path, "moved.mp4");
        drop(legacy_guard);
        drop(lease);
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cached_keyframe_lease_blocks_mutation_until_reader_finishes() {
        let (root, catalog, handle) = fixture("reader-keyframe-lease", true);
        let fragments = handle
            .media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        let location = super::super::EventKeyframeLocation {
            event_id: "event".into(),
            stream_id: "main".into(),
            event_time_ms: 1000,
            recording_id: "reader-recording".into(),
            fragment_sequence: 1,
            fragment_start_ms: 1000,
            path: fragments[0].path.clone(),
            byte_offset: 40,
            byte_len: 30,
        };
        let lease = handle.lease_event_keyframe(&location).unwrap();
        assert!(handle.delete_recording("reader-recording").is_err());
        assert!(
            handle
                .reader_leases()
                .conflicts("reader-recording", "")
                .unwrap()
        );
        drop(lease);
        assert!(
            !handle
                .reader_leases()
                .conflicts("reader-recording", "")
                .unwrap()
        );
        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn named_reader_retains_catalog_authority_after_shutdown() {
        let (root, catalog, handle) = fixture("reader-leases-authority", true);
        let (_, lease) = handle
            .leased_media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        catalog.shutdown();
        assert!(RecordingCatalog::open(&root.join("recordings.db")).is_err());
        drop(lease);
        let reopened = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        reopened.shutdown();
        drop(handle);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn abandoned_actor_reply_releases_reader_lease() {
        let (root, catalog, handle) = fixture("reader-leases-abandoned", true);
        let (reply, response) = mpsc::sync_channel(1);
        drop(response);
        handle
            .tx
            .send(Command::ReadLease {
                request: Request::Fragments {
                    stream: "camera/main".into(),
                    start: 1000,
                    end: 3000,
                },
                reply,
            })
            .unwrap();
        // The following actor command waits until the abandoned reply was dropped.
        handle
            .media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        assert!(
            !handle
                .reader_leases()
                .conflicts("reader-recording", "")
                .unwrap()
        );
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn move_executor_is_shared_between_handles_until_worker_returns() {
        let (root, catalog, handle) = fixture("move-worker-lease-duplicate", false);
        let worker_handle = handle.clone();
        let (ready, started) = mpsc::sync_channel(1);
        let (release, finish) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _lease = worker_handle.claim_volume_move("move-worker").unwrap();
            ready.send(()).unwrap();
            finish
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        });
        started
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(handle.claim_volume_move("move-worker").is_err());
        assert!(handle.claim_volume_move("").is_err());
        assert!(handle.claim_volume_move(&"a".repeat(257)).is_err());
        release.send(()).unwrap();
        worker.join().unwrap();
        let lease = handle.claim_volume_move("move-worker").unwrap();
        drop(lease);
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn move_worker_retains_authority_across_catalog_shutdown() {
        let (root, catalog, handle) = fixture("move-worker-lease-authority", false);
        let lease = handle.claim_volume_move("move-worker").unwrap();
        catalog.shutdown();
        assert!(RecordingCatalog::open(&root.join("recordings.db")).is_err());
        drop(lease);
        assert!(handle.claim_volume_move("move-worker").is_err());
        let reopened = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        reopened.shutdown();
        drop(handle);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn move_worker_capacity_reaps_only_released_entries() {
        let (root, catalog, handle) = fixture("move-worker-lease-capacity", false);
        let mut leases = (0..MAX_MOVE_WORKERS)
            .map(|index| handle.claim_volume_move(&format!("move-{index}")).unwrap())
            .collect::<Vec<_>>();
        assert!(handle.claim_volume_move("overflow").is_err());
        assert!(handle.claim_volume_move("move-0").is_err());
        drop(leases.pop());
        let replacement = handle.claim_volume_move("replacement").unwrap();
        assert!(handle.claim_volume_move("overflow").is_err());
        drop(replacement);
        drop(leases);
        let reopened = handle.claim_volume_move("move-0").unwrap();
        assert_eq!(handle.readers.moves.lock().unwrap().len(), 1);
        drop(reopened);
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_path_alias_reader_leases_the_owned_location() {
        let (root, catalog, handle) = fixture("reader-leases-path-alias", true);
        let original = handle
            .media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        let alias_path = alternate_separators(&original[0].path);
        insert_alias(&handle, &alias_path);
        let (_, lease) = handle
            .leased_media_fragments_in_range("alias/main", 1000, 3000)
            .unwrap();
        assert!(
            handle
                .reader_leases()
                .conflicts("reader-recording", &original[0].path)
                .unwrap()
        );
        assert!(
            handle
                .reader_leases()
                .conflicts("legacy-alias", "")
                .unwrap()
        );
        drop(lease);
        assert!(
            !handle
                .reader_leases()
                .conflicts("reader-recording", &alias_path)
                .unwrap()
        );
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn alternate_separators(path: &str) -> String {
        let alias = if path.contains('\\') {
            path.replace('\\', "/")
        } else {
            path.replace('/', "\\")
        };
        assert_ne!(alias, path, "alias must remain a distinct catalog spelling");
        alias
    }

    fn insert_alias(handle: &RecordingCatalogHandle, alias_path: &str) {
        handle
            .upsert_recording(CatalogRecording {
                id: "legacy-alias".into(),
                stream_id: "alias/main".into(),
                source_id: None,
                logical_stream_id: None,
                started_at_ms: 1000,
                ended_at_ms: Some(3000),
                path: alias_path.to_owned(),
                init_offset: 0,
                init_len: 20,
                finalized: true,
            })
            .unwrap();
        handle
            .insert_fragment(CatalogFragment {
                recording_id: "legacy-alias".into(),
                sequence: 1,
                start_ms: 1000,
                duration_ms: 2000,
                byte_offset: 20,
                byte_len: 100,
                random_access: true,
            })
            .unwrap();
    }

    fn publish_reader_move(handle: &RecordingCatalogHandle, root: &std::path::Path) {
        use super::super::locations::{
            Allocation, Binding, Capacity, Kind, Object, Publication, Request, moves,
        };
        handle
            .volume_location(Request::Publish(Publication {
                operation: "reader-allocation".into(),
                bytes: 120,
                file_identity: "source-file".into(),
                digest: [7; 32],
            }))
            .unwrap();
        handle
            .volume_location(Request::Bind(Binding {
                id: "destination".into(),
                generation: 1,
                root: root.join("destination"),
                filesystem: "readers-fs".into(),
                root_identity: "destination-root".into(),
                writable: true,
                draining: false,
                limit_bytes: Some(4096),
                minimum_free_bytes: 0,
            }))
            .unwrap();
        let intent = moves::Intent {
            id: "reader-move".into(),
            object: Object {
                kind: Kind::Recording,
                id: "reader-recording".into(),
            },
            expected_revision: 1,
            destination: Allocation {
                operation: "reader-move".into(),
                object: Object {
                    kind: Kind::Recording,
                    id: "reader-move".into(),
                },
                volume: "destination".into(),
                generation: 1,
                relative_key: "move.mp4".into(),
                bytes: 120,
                capacity: Capacity {
                    ledger_revision: handle.volume_ledger_revision().unwrap(),
                    observed_at: std::time::Instant::now(),
                    available_bytes: 4096,
                    filesystem: "readers-fs".into(),
                    root_identity: "destination-root".into(),
                },
            },
        };
        handle.volume_location(Request::BeginMove(intent)).unwrap();
        for step in [
            moves::Step::Verified(Publication {
                operation: "reader-move".into(),
                bytes: 120,
                file_identity: "destination-file".into(),
                digest: [7; 32],
            }),
            moves::Step::FilePublished("reader-move".into()),
            moves::Step::Publish("reader-move".into()),
        ] {
            handle.volume_location(Request::AdvanceMove(step)).unwrap();
        }
    }

    #[test]
    fn published_move_rejects_new_alias_readers_but_retains_existing_guards() {
        let (root, catalog, handle) = fixture("reader-leases-retired-alias", true);
        let original = handle
            .media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        let alias = alternate_separators(&original[0].path);
        insert_alias(&handle, &alias);
        let (old_snapshots, old_lease) = handle
            .leased_media_fragments_in_range("alias/main", 1000, 3000)
            .unwrap();
        publish_reader_move(&handle, &root);
        assert!(
            handle
                .reader_leases()
                .conflicts("reader-recording", &original[0].path)
                .unwrap()
        );
        assert!(
            handle
                .leased_media_fragments_in_range("alias/main", 1000, 3000)
                .is_err()
        );
        assert!(handle.lease_media_fragments(&old_snapshots).is_err());
        let (current, current_lease) = handle
            .leased_media_fragments_in_range("camera/main", 1000, 3000)
            .unwrap();
        assert_ne!(current[0].path, original[0].path);
        assert_eq!(current[0].recording_id, "reader-recording");
        drop(current_lease);
        drop(old_lease);
        assert!(
            !handle
                .reader_leases()
                .conflicts("reader-recording", &original[0].path)
                .unwrap()
        );
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(root).unwrap();
    }
}
