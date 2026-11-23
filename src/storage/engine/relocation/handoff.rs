use super::{
    RecordingCatalogHandle, StorageConfig,
    files::{check_running, remove_stage, same_entry, stage_source, staged_name, verify_existing},
    legacy_stream,
};
use crate::storage::catalog::relocation::Move;
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, atomic::AtomicBool},
};
pub(in crate::storage::engine) fn move_recording(
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    camera_id: &str,
    path: &Path,
    recording_id: &str,
    stop: &AtomicBool,
) -> io::Result<PathBuf> {
    check_running(stop)?;
    if wake_managed_volume(config, catalog, recording_id)? {
        return Ok(path.to_owned());
    }
    if config.medium_term_path == config.long_term_path {
        return Ok(path.to_owned());
    }
    let relative = path
        .strip_prefix(&config.medium_term_path)
        .map_err(io::Error::other)?;
    let destination = config.long_term_path.join(relative);
    if catalog.is_none() && same_entry(path, &destination)? {
        return Ok(destination);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("missing recording directory"))?;
    fs::create_dir_all(parent)?;
    if let Some(catalog) = catalog {
        if catalog
            .reader_leases()
            .conflicts(recording_id, &path.to_string_lossy())
            .map_err(io::Error::other)?
        {
            return Err(io::Error::other(
                "active recording reader defers relocation",
            ));
        }
        if destination.try_exists()? && !same_entry(path, &destination)? {
            verify_existing(path, &destination, stop)?;
        }
        let metadata = fs::symlink_metadata(path)?;
        let identity = crate::storage::catalog::recording_file_identity(path, &metadata);
        let job = catalog
            .reserve_relocation(Move {
                id: recording_id.to_owned(),
                stream: camera_id.to_owned(),
                source: path.to_owned(),
                destination: destination.clone(),
                staged: uuid::Uuid::new_v4(),
                destination_published: false,
                source_bytes: metadata.len(),
                source_identity: identity
                    .ok_or_else(|| io::Error::other("recording identity unavailable"))?,
            })
            .map_err(io::Error::other)?;
        recover_job(catalog, &job, stop)?;
    } else {
        // The camera diagnostic runner has no catalog or retention writer. Derive the stage
        // from the immutable finalized source so a restart can reclaim its own incomplete copy.
        use sha2::{Digest, Sha256};
        let identity =
            crate::storage::catalog::recording_file_identity(path, &fs::symlink_metadata(path)?)
                .ok_or_else(|| io::Error::other("recording identity unavailable"))?;
        let digest = Sha256::digest(identity.as_bytes());
        let stage =
            uuid::Uuid::from_bytes(digest[..16].try_into().expect("SHA-256 contains 16 bytes"));
        let _attempt = Attempt::acquire(stage)?;
        stage_source(path, &destination, stage, stop)?;
        check_running(stop)?;
        fs::remove_file(path)?;
    }
    Ok(destination)
}

fn wake_managed_volume(
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    recording_id: &str,
) -> io::Result<bool> {
    if config.volume_runtime.is_some() {
        use crate::storage::catalog::locations::{Kind, Object, Reply, Request};
        if let Some(catalog) = catalog
            && matches!(
                catalog
                    .volume_location(Request::Lookup(Object {
                        kind: Kind::Recording,
                        id: recording_id.to_owned()
                    }))
                    .map_err(std::io::Error::other)?,
                Reply::Location(Some(_))
            )
        {
            if let Some(mover) = &config.volume_mover {
                mover.scan().map_err(std::io::Error::other)?;
            }
            if let Some(archive) = &config.archive_runtime
                && let Err(error) = archive.scan()
            {
                tracing::warn!(%error,"archive wakeup deferred; finalized recording remains available locally");
            }
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn recover_job(
    catalog: &RecordingCatalogHandle,
    job: &Move,
    stop: &AtomicBool,
) -> io::Result<()> {
    check_running(stop)?;
    let _attempt = Attempt::acquire(job.staged)?;
    validate_reserved_paths(job)?;
    let staged = job.destination.with_file_name(staged_name(job.staged));
    // The UUID was reserved by the catalog before even one media byte could be written.
    if !job.source.try_exists()? {
        if !job.destination_published || !job.destination.try_exists()? {
            return Err(io::Error::other(
                "source disappeared before recording handoff",
            ));
        }
        // Keep the only remaining staging bytes through source/target outages or replacement.
        catalog
            .verify_published_relocation(job)
            .map_err(io::Error::other)?;
        protect_complete_stage(&job.destination, &staged, job.source_bytes, stop)?;
        remove_stage(&staged)?;
        sync_parent(&job.source)?;
        return catalog.retire_relocation(job).map_err(io::Error::other);
    }
    verify_source(job)?;
    protect_complete_stage(&job.source, &staged, job.source_bytes, stop)?;
    if job.destination_published && job.destination.try_exists()? {
        catalog
            .verify_published_relocation(job)
            .map_err(io::Error::other)?;
    }
    if same_entry(&job.source, &job.destination)? {
        if !job.destination_published {
            catalog
                .publish_relocation(job, job.source_identity.clone())
                .map_err(io::Error::other)?;
        }
        remove_stage(&staged)?;
        return catalog.retire_relocation(job).map_err(io::Error::other);
    }
    if job.destination_published && job.destination.try_exists()? {
        verify_existing(&job.source, &job.destination, stop)?;
    } else {
        if catalog
            .reader_leases()
            .conflicts(&job.id, &job.source.to_string_lossy())
            .map_err(io::Error::other)?
        {
            return Err(io::Error::other(
                "active recording reader defers relocation",
            ));
        }
        stage_source(&job.source, &job.destination, job.staged, stop)?;
        sync_parent(&job.destination)?;
        check_running(stop)?;
        let identity = qualified_identity(&job.destination, job.source_bytes)?;
        if identity != job.source_identity {
            verify_existing(&job.source, &job.destination, stop)?;
        }
        verify_source(job)?;
        catalog
            .publish_relocation(job, identity)
            .map_err(io::Error::other)?;
    }
    catalog
        .verify_published_relocation(job)
        .map_err(io::Error::other)?;
    remove_stage(&staged)?;
    if same_entry(&job.source, &job.destination)? {
        return catalog.retire_relocation(job).map_err(io::Error::other);
    }
    verify_source(job)?;
    check_running(stop)?;
    fs::remove_file(&job.source)?;
    sync_parent(&job.source)?;
    catalog.retire_relocation(job).map_err(io::Error::other)
}

fn validate_reserved_paths(job: &Move) -> io::Result<()> {
    // The catalog reserved these endpoints before publication. Complete that original
    // handoff across a configuration change without deriving a third path from new roots.
    let depth = Path::new(&job.stream).components().count() + 3;
    let source_root = job.source.ancestors().nth(depth);
    let destination_root = job.destination.ancestors().nth(depth);
    if !Path::new(&job.stream)
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
        || !source_root
            .zip(destination_root)
            .is_some_and(|(source, destination)| {
                legacy_stream(source, &job.source).as_deref() == Some(&job.stream)
                    && job.source.strip_prefix(source).ok()
                        == job.destination.strip_prefix(destination).ok()
            })
    {
        return Err(io::Error::other("invalid recording relocation endpoints"));
    }
    Ok(())
}

fn qualified_identity(path: &Path, bytes: u64) -> io::Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() != bytes {
        return Err(io::Error::other(
            "recording length changed during relocation",
        ));
    }
    crate::storage::catalog::recording_file_identity(path, &meta)
        .ok_or_else(|| io::Error::other("recording identity unavailable"))
}

fn verify_source(job: &Move) -> io::Result<()> {
    if qualified_identity(&job.source, job.source_bytes)? != job.source_identity {
        return Err(io::Error::other(
            "recording source changed during relocation",
        ));
    }
    Ok(())
}

fn protect_complete_stage(
    reference: &Path,
    staged: &Path,
    expected: u64,
    stop: &AtomicBool,
) -> io::Result<()> {
    match fs::symlink_metadata(staged) {
        Ok(metadata) if metadata.is_file() && metadata.len() >= expected => {
            verify_existing(reference, staged, stop)
        }
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(io::Error::other("recording stage is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing recording directory"))?;
    let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    crate::storage::long_term::inspection::removal::sync_directory(&directory)
}

// A kernel I/O call can outlive orderly shutdown. A restarted worker retries this
// one journal identity after the old call returns; unrelated recordings remain movable.
static ACTIVE: LazyLock<Mutex<HashSet<uuid::Uuid>>> = LazyLock::new(Default::default);
struct Attempt(uuid::Uuid);

impl Attempt {
    fn acquire(id: uuid::Uuid) -> io::Result<Self> {
        if !ACTIVE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "recording relocation still in progress",
            ));
        }
        Ok(Self(id))
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        ACTIVE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}
