//! Confined writers backed by durable, actor-serialized capacity reservations.

use super::{
    PlacementRequest, Volume, VolumeConfiguration, VolumeHealth, VolumeObservation, VolumeRole,
    VolumeState,
    root::{OwnedFile, Root},
};
use crate::storage::catalog::{
    RecordingCatalogHandle,
    locations::{
        Allocation, Binding, Growth, Kind, Materialization, Object, Publication, Reply, Request,
        Usage,
    },
};
use std::{
    cell::{Cell, RefCell},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

const GROWTH_BYTES: u64 = 1_048_576;

mod cancellation;
mod movement;
#[cfg(test)]
mod movement_tests;
mod retirement;
pub mod worker;

/// Shares admission serialization and pinned roots among local writers.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    configuration: VolumeConfiguration,
    catalog: RecordingCatalogHandle,
    roots: Vec<OnceLock<Root>>,
    admission: Mutex<()>,
}

/// Durable capacity ownership that has not yet created its file.
pub struct Reservation {
    inner: Arc<Inner>,
    index: usize,
    operation: String,
    key: String,
    path: PathBuf,
    bytes: u64,
}

/// A fixed-volume writer that reserves capacity before extending its file.
pub struct ReservedFile {
    reservation: Reservation,
    file: OwnedFile,
    evidence: RefCell<Option<Publication>>,
    published: Cell<bool>,
    failed: bool,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("volumes", &self.inner.configuration.volumes.len())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ReservedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReservedFile")
            .field("reservation", &self.reservation)
            .field("published", &self.published.get())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl Manager {
    /// Reserves an archive copy before it enters the bounded worker queue.
    ///
    /// # Errors
    /// Refuses stale ownership or a policy without an available destination.
    pub fn schedule_move(
        &self,
        job_id: &str,
        object: Object,
        request: &PlacementRequest<'_>,
        groups: &[&str],
    ) -> anyhow::Result<bool> {
        Ok(self
            .reserve_move(job_id, object, request, groups)?
            .is_some())
    }

    /// Copies an owned object to its resolved destination and publishes its stable identity.
    /// The old copy remains owned until a separate reader-aware retirement completes.
    ///
    /// # Errors
    /// Refuses stale ownership, unavailable destinations, cancellation, or failed verification.
    pub fn move_object(
        &self,
        job_id: &str,
        object: Object,
        request: &PlacementRequest<'_>,
        groups: &[&str],
        cancelled: impl Fn() -> bool,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(!cancelled(), "move cancelled");
        let Some((source, destination)) = self.reserve_move(job_id, object, request, groups)?
        else {
            return Ok(false);
        };
        self.copy_move(job_id, &source, destination, cancelled)?;
        Ok(true)
    }

    fn reserve_move(
        &self,
        job_id: &str,
        object: Object,
        request: &PlacementRequest<'_>,
        groups: &[&str],
    ) -> anyhow::Result<Option<(crate::storage::catalog::locations::Location, Reservation)>> {
        anyhow::ensure!(uuid::Uuid::parse_str(job_id).is_ok(), "invalid move job ID");
        anyhow::ensure!(groups.len() <= super::RULES_MAX, "too many source groups");
        let _guard = self
            .inner
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("volume admission unavailable"))?;
        let Reply::Location(Some(source)) = self
            .inner
            .catalog
            .volume_location(Request::Lookup(object.clone()))?
        else {
            anyhow::bail!("move source is not owned");
        };
        let request = PlacementRequest {
            required_bytes: source.bytes,
            ..*request
        };
        let Some(rule) = self.inner.configuration.matching_rule(&request, groups) else {
            return Ok(None);
        };
        let observations = self.inner.observations(&rule.candidates)?;
        let decision =
            self.inner
                .configuration
                .place_with_groups(&request, groups, &observations)?;
        let selected = decision
            .selected
            .ok_or_else(|| anyhow::anyhow!("move policy has no writable destination"))?;
        if selected.as_str() == source.volume {
            return Ok(None);
        }
        let index = self
            .inner
            .configuration
            .volumes
            .iter()
            .position(|volume| volume.id == selected)
            .expect("configured destination");
        self.commit_move(index, source, object, job_id, request.role)
            .map(Some)
    }

    fn commit_move(
        &self,
        index: usize,
        source: crate::storage::catalog::locations::Location,
        object: Object,
        job_id: &str,
        role: VolumeRole,
    ) -> anyhow::Result<(crate::storage::catalog::locations::Location, Reservation)> {
        use crate::storage::catalog::locations::moves::Intent;
        let volume = &self.inner.configuration.volumes[index];
        let key = object_key(
            role,
            &Object {
                kind: object.kind,
                id: job_id.to_owned(),
            },
        )?;
        let destination = Allocation {
            operation: job_id.to_owned(),
            object: Object {
                kind: object.kind,
                id: job_id.to_owned(),
            },
            volume: volume.id.to_string(),
            generation: 1,
            relative_key: key.clone(),
            bytes: source.bytes,
            capacity: self
                .inner
                .root(index)?
                .capacity(self.inner.catalog.volume_ledger_revision()?)?,
        };
        let Reply::Move(job) = self
            .inner
            .catalog
            .volume_location(Request::BeginMove(Intent {
                id: job_id.to_owned(),
                object,
                expected_revision: source.revision,
                destination,
            }))?
        else {
            anyhow::bail!("invalid move admission reply");
        };
        anyhow::ensure!(
            job.phase == "reserved",
            "move requires recovery of its durable phase"
        );
        Ok((
            source.clone(),
            Reservation {
                inner: Arc::clone(&self.inner),
                index,
                operation: job_id.to_owned(),
                path: volume.root.join(&key),
                key,
                bytes: source.bytes,
            },
        ))
    }

    /// Checks whether a published move's source has no remaining reader workers.
    /// This does not authorize deletion or substitute for pinned file validation.
    ///
    /// # Errors
    /// Rejects unavailable journals, changed authority, and unknown source roots.
    pub fn retirement_ready(&self, job_id: &str) -> anyhow::Result<bool> {
        let Reply::Move(job) = self
            .inner
            .catalog
            .volume_location(Request::Move(job_id.to_owned()))?
        else {
            anyhow::bail!("invalid move journal reply");
        };
        anyhow::ensure!(
            matches!(job.phase.as_str(), "published" | "retiring"),
            "move has no published destination"
        );
        let Reply::Location(Some(current)) = self
            .inner
            .catalog
            .volume_location(Request::Lookup(job.object.clone()))?
        else {
            anyhow::bail!("move destination is not authoritative");
        };
        anyhow::ensure!(
            current.revision > job.source.revision,
            "move source is still authoritative"
        );
        let volume = self
            .inner
            .configuration
            .volumes
            .iter()
            .find(|volume| volume.id.as_str() == job.source.volume)
            .ok_or_else(|| anyhow::anyhow!("move source volume is not configured"))?;
        let path = volume.root.join(&job.source.relative_key);
        Ok(!self
            .inner
            .catalog
            .reader_leases()
            .conflicts(&job.object.id, &path.to_string_lossy())?)
    }

    /// Opens and binds qualified roots; an unavailable root is isolated from placement.
    ///
    /// # Errors
    /// Rejects invalid configuration. Individual root failures leave that root offline.
    pub fn new(
        configuration: VolumeConfiguration,
        catalog: RecordingCatalogHandle,
    ) -> anyhow::Result<Self> {
        configuration.validate()?;
        let roots = configuration
            .volumes
            .iter()
            .map(|volume| {
                let slot = OnceLock::new();
                if volume.state == VolumeState::Disabled {
                    return slot;
                }
                if let Ok(root) = bind_root(volume, &catalog) {
                    slot.set(root).expect("new root slot is empty");
                }
                slot
            })
            .collect();
        Ok(Self {
            inner: Arc::new(Inner {
                configuration,
                catalog,
                roots,
                admission: Mutex::new(()),
            }),
        })
    }

    fn recover_roots(&self) -> anyhow::Result<()> {
        // ponytail: Retry at most 32 startup-offline roots in the existing journal scan.
        for (index, volume) in self.inner.configuration.volumes.iter().enumerate() {
            let slot = &self.inner.roots[index];
            if volume.state == VolumeState::Disabled || slot.get().is_some() {
                continue;
            }
            let root = match open_root(volume) {
                Ok(root) => root,
                Err(_) => continue,
            };
            let _guard = self
                .inner
                .admission
                .lock()
                .map_err(|_| anyhow::anyhow!("volume admission unavailable"))?;
            if slot.get().is_some() {
                continue;
            }
            match bind_opened_root(volume, &root, &self.inner.catalog) {
                Ok(()) => {
                    slot.set(root)
                        .expect("admission lock protects empty root slot");
                    tracing::info!(volume_id = %volume.id, "storage volume recovered");
                }
                Err(_) => tracing::warn!(volume_id = %volume.id, "storage volume recovery refused"),
            }
        }
        Ok(())
    }

    /// Reserves one configured destination; only an unmatched rule returns `None`.
    ///
    /// # Errors
    /// Rejects unavailable configured policies, invalid objects, or refused admission.
    pub fn reserve(
        &self,
        role: VolumeRole,
        source: &str,
        groups: &[&str],
        object: Object,
        required_bytes: u64,
    ) -> anyhow::Result<Option<Reservation>> {
        let request = PlacementRequest {
            role,
            source,
            group: "",
            required_bytes,
        };
        anyhow::ensure!(groups.len() <= super::RULES_MAX, "too many source groups");
        let Some(rule) = self.inner.configuration.matching_rule(&request, groups) else {
            return Ok(None);
        };
        let key = object_key(role, &object)?;
        let _guard = self
            .inner
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("volume admission unavailable"))?;
        let observations = self.inner.observations(&rule.candidates)?;
        let decision =
            self.inner
                .configuration
                .place_with_groups(&request, groups, &observations)?;
        let selected = decision.selected.ok_or_else(|| {
            anyhow::anyhow!("configured volume policy has no writable destination")
        })?;
        let index = self
            .inner
            .configuration
            .volumes
            .iter()
            .position(|volume| volume.id == selected)
            .expect("placement selects configured volume");
        self.reserve_on_volume(index, object, key, required_bytes)
            .map(Some)
    }

    fn reserve_on_volume(
        &self,
        index: usize,
        object: Object,
        key: String,
        required_bytes: u64,
    ) -> anyhow::Result<Reservation> {
        let volume = &self.inner.configuration.volumes[index];
        let root = self.inner.root(index)?;
        let capacity = root.capacity(self.inner.catalog.volume_ledger_revision()?)?;
        let operation = uuid::Uuid::new_v4().to_string();
        let request = Request::Reserve(Allocation {
            operation: operation.clone(),
            object,
            volume: volume.id.to_string(),
            generation: 1,
            relative_key: key.clone(),
            bytes: required_bytes,
            capacity,
        });
        let reply = self
            .inner
            .catalog
            .volume_location(request.clone())
            .or_else(|_| self.inner.catalog.volume_location(request))?;
        let Reply::Reserved { bytes, .. } = reply else {
            anyhow::bail!("invalid reservation reply");
        };
        Ok(Reservation {
            inner: Arc::clone(&self.inner),
            index,
            operation,
            path: volume.root.join(&key),
            key,
            bytes,
        })
    }
}

fn bind_root(volume: &Volume, catalog: &RecordingCatalogHandle) -> anyhow::Result<Root> {
    let root = open_root(volume)?;
    bind_opened_root(volume, &root, catalog)?;
    Ok(root)
}

fn open_root(volume: &Volume) -> anyhow::Result<Root> {
    let root = Root::open(&volume.root)?;
    if volume.state == VolumeState::Enabled {
        root.sync()?;
    }
    Ok(root)
}

fn bind_opened_root(
    volume: &Volume,
    root: &Root,
    catalog: &RecordingCatalogHandle,
) -> anyhow::Result<()> {
    catalog.volume_location(Request::Bind(Binding {
        id: volume.id.to_string(),
        generation: 1,
        root: volume.root.clone(),
        filesystem: root.identity().filesystem.clone(),
        root_identity: root.identity().directory.clone(),
        writable: matches!(volume.state, VolumeState::Enabled | VolumeState::Draining),
        draining: volume.state == VolumeState::Draining,
        limit_bytes: volume.capacity_bytes,
        minimum_free_bytes: volume.minimum_free_bytes.max(volume.critical_free_bytes),
    }))?;
    Ok(())
}

fn object_key(role: VolumeRole, object: &Object) -> anyhow::Result<String> {
    let extension = match (role, object.kind) {
        (VolumeRole::Active | VolumeRole::Archive, Kind::Recording)
        | (VolumeRole::Export, Kind::Export) => "mp4",
        (VolumeRole::Thumbnail, Kind::Thumbnail) => "jpg",
        _ => anyhow::bail!("object kind does not match volume role"),
    };
    anyhow::ensure!(
        matches!(object.id.len(), 32 | 36) && uuid::Uuid::parse_str(&object.id).is_ok(),
        "object ID must be a UUID"
    );
    Ok(format!("{}.{extension}", object.id))
}

impl Inner {
    fn writable_root(&self, index: usize) -> anyhow::Result<&Root> {
        anyhow::ensure!(
            matches!(
                self.configuration.volumes[index].state,
                VolumeState::Enabled | VolumeState::Draining
            ),
            "volume does not permit changes to existing objects"
        );
        self.root(index)
    }

    fn root(&self, index: usize) -> anyhow::Result<&Root> {
        self.roots[index]
            .get()
            .ok_or_else(|| anyhow::anyhow!("configured volume is offline"))
    }

    fn observations(
        &self,
        candidates: &[super::VolumeId],
    ) -> anyhow::Result<Vec<VolumeObservation>> {
        let Reply::Usage(usage) = self.catalog.volume_location(Request::Usage)? else {
            anyhow::bail!("invalid volume usage reply");
        };
        let revision = self.catalog.volume_ledger_revision()?;
        self.configuration
            .volumes
            .iter()
            .enumerate()
            .filter(|(_, volume)| candidates.contains(&volume.id))
            .map(|(index, volume)| {
                let sample = self.root(index).and_then(|root| root.capacity(revision));
                let owned_bytes = usage
                    .iter()
                    .find(|item| item.volume == volume.id.as_str())
                    .map_or(0, |item| item.allocated_bytes);
                let (health, available_bytes) = match sample {
                    Ok(sample) => (
                        VolumeHealth::Online,
                        sample
                            .available_bytes
                            .saturating_sub(reserved_on_filesystem(&usage, &sample.filesystem)?),
                    ),
                    Err(_) => (VolumeHealth::Offline, 0),
                };
                // Total capacity is unavailable; this probe reports only caller-available bytes.
                Ok(VolumeObservation {
                    id: volume.id.clone(),
                    health,
                    total_bytes: u64::MAX,
                    available_bytes,
                    owned_bytes,
                })
            })
            .collect()
    }
}

fn reserved_on_filesystem(usage: &[Usage], filesystem: &str) -> anyhow::Result<u64> {
    usage
        .iter()
        .filter(|item| item.filesystem == filesystem)
        .try_fold(0_u64, |total, item| {
            total
                .checked_add(item.reserved_bytes)
                .ok_or_else(|| anyhow::anyhow!("filesystem reservation overflow"))
        })
}

impl Reservation {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates the exclusively owned file after its reservation has committed.
    ///
    /// # Errors
    /// Rejects changed roots or conflicting leaves; ownership remains reserved on failure.
    pub fn open(self) -> anyhow::Result<ReservedFile> {
        let file = self
            .inner
            .writable_root(self.index)?
            .create_file(&self.key)?;
        let mut writer = ReservedFile {
            reservation: self,
            file,
            evidence: RefCell::new(None),
            published: Cell::new(false),
            failed: false,
        };
        writer.checkpoint()?;
        Ok(writer)
    }
}

impl ReservedFile {
    /// Synchronizes the pinned file and captures evidence for publication.
    ///
    /// # Errors
    /// Rejects changed files, unavailable roots, or failed synchronization.
    pub fn evidence(&mut self) -> anyhow::Result<Publication> {
        anyhow::ensure!(!self.failed, "failed writer cannot publish");
        let (bytes, file_identity, digest) = self.file.evidence()?;
        let publication = Publication {
            operation: self.reservation.operation.clone(),
            bytes,
            file_identity,
            digest,
        };
        *self.evidence.borrow_mut() = Some(publication.clone());
        Ok(publication)
    }

    /// Publishes captured evidence after the recording owner finalizes its catalog row.
    ///
    /// # Errors
    /// Rejects stale evidence, changed names, or a refused catalog transition.
    pub fn publish(&self, publication: Publication) -> anyhow::Result<()> {
        self.commit_publication(publication, false)
    }

    /// Finalizes recording metadata and publishes its location in one transaction.
    ///
    /// # Errors
    /// Rejects stale evidence, changed names, or a refused catalog transition.
    pub fn finalize(&self, publication: Publication) -> anyhow::Result<()> {
        self.commit_publication(publication, true)
    }

    fn commit_publication(&self, publication: Publication, finalize: bool) -> anyhow::Result<()> {
        anyhow::ensure!(!self.failed, "failed writer cannot publish");
        self.file.revalidate()?;
        anyhow::ensure!(
            self.evidence.borrow().as_ref() == Some(&publication),
            "publication does not match captured evidence"
        );
        let reply = self
            .reservation
            .inner
            .catalog
            .volume_location(if finalize {
                Request::Finalize(publication)
            } else {
                Request::Publish(publication)
            })?;
        anyhow::ensure!(
            matches!(reply, Reply::Location(Some(_))),
            "publication returned no location"
        );
        self.published.set(true);
        Ok(())
    }

    fn grow(&mut self, needed: u64) -> anyhow::Result<()> {
        if needed <= self.reservation.bytes {
            return Ok(());
        }
        anyhow::ensure!(
            needed - self.reservation.bytes <= GROWTH_BYTES,
            "write exceeds bounded growth increment"
        );
        self.checkpoint()?;
        let inner = &self.reservation.inner;
        let _guard = inner
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("volume admission unavailable"))?;
        let volume = &inner.configuration.volumes[self.reservation.index];
        let cap = volume
            .capacity_bytes
            .unwrap_or(i64::MAX as u64)
            .min(i64::MAX as u64);
        anyhow::ensure!(needed <= cap, "write exceeds volume capacity");
        let rounded = needed
            .div_ceil(GROWTH_BYTES)
            .checked_mul(GROWTH_BYTES)
            .unwrap_or(cap)
            .min(cap)
            .min(self.reservation.bytes.saturating_add(GROWTH_BYTES));
        let first = self.request_growth(rounded);
        let bytes = match first {
            Ok(bytes) => bytes,
            Err(_) => self.request_growth(needed)?,
        };
        self.reservation.bytes = bytes;
        Ok(())
    }

    fn request_growth(&self, bytes: u64) -> anyhow::Result<u64> {
        let reservation = &self.reservation;
        let root = reservation.inner.root(reservation.index)?;
        let capacity = root.capacity(reservation.inner.catalog.volume_ledger_revision()?)?;
        let reply = reservation
            .inner
            .catalog
            .volume_location(Request::Grow(Growth {
                operation: reservation.operation.clone(),
                bytes,
                capacity,
            }))?;
        let Reply::Reserved { bytes, .. } = reply else {
            anyhow::bail!("invalid growth reply");
        };
        Ok(bytes)
    }

    fn checkpoint(&mut self) -> anyhow::Result<()> {
        let result = (|| {
            let (bytes, file_identity) = self.file.checkpoint()?;
            self.reservation
                .inner
                .catalog
                .volume_location(Request::Materialize(Materialization {
                    operation: self.reservation.operation.clone(),
                    bytes,
                    file_identity,
                }))?;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

impl Write for ReservedFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.published.get() || self.failed {
            return Err(io::Error::other("sealed or failed files cannot be changed"));
        }
        let result = (|| {
            self.reservation
                .inner
                .writable_root(self.reservation.index)
                .map_err(io::Error::other)?;
            self.file.revalidate().map_err(io::Error::other)?;
            if buffer.is_empty() {
                return Ok(0);
            }
            let amount = buffer.len().min(GROWTH_BYTES as usize);
            let needed = self
                .file
                .file_mut()
                .stream_position()?
                .checked_add(amount as u64)
                .ok_or_else(|| io::Error::other("file position overflow"))?;
            self.grow(needed).map_err(io::Error::other)?;
            *self.evidence.borrow_mut() = None;
            self.file.file_mut().write(&buffer[..amount])
        })();
        // A buffered owner's drop must not replay a failed write into this file.
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.file_mut().flush()
    }
}

impl Seek for ReservedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let file = self.file.file_mut();
        let length = file.metadata()?.len();
        let destination = match position {
            SeekFrom::Start(value) => Some(value),
            SeekFrom::End(delta) => length.checked_add_signed(delta),
            SeekFrom::Current(delta) => file.stream_position()?.checked_add_signed(delta),
        }
        .filter(|value| *value <= length)
        .ok_or_else(|| io::Error::other("seek exceeds materialized file"))?;
        file.seek(SeekFrom::Start(destination))
    }
}

#[cfg(test)]
pub(crate) mod tests;
