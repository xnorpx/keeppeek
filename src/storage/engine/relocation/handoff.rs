use super::{
    RecordingCatalogHandle, StorageConfig,
    files::{remove_stage, same_entry, stage_source, staged_name, verify_existing},
    legacy_stream,
};
use crate::storage::catalog::relocation::Move;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
pub(in crate::storage::engine) fn move_recording(
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    camera_id: &str,
    path: &Path,
    recording_id: &str,
) -> io::Result<PathBuf> {
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
            return Ok(path.to_path_buf());
        }
    }
    if config.medium_term_path == config.long_term_path {
        return Ok(path.to_owned());
    }
    let relative = path
        .strip_prefix(&config.medium_term_path)
        .map_err(io::Error::other)?;
    let destination = config.long_term_path.join(relative);
    if same_entry(path, &destination)? {
        return Ok(path.to_owned());
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
        if destination.try_exists()? {
            verify_existing(path, &destination)?;
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
                source_identity: identity
                    .ok_or_else(|| io::Error::other("recording identity unavailable"))?,
            })
            .map_err(io::Error::other)?;
        recover_job(config, catalog, &job)?;
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
        stage_source(path, &destination, stage)?;
        fs::remove_file(path)?;
    }
    Ok(destination)
}

pub(super) fn recover_job(
    config: &StorageConfig,
    catalog: &RecordingCatalogHandle,
    job: &Move,
) -> io::Result<()> {
    let tail = job
        .source
        .strip_prefix(&config.medium_term_path)
        .map_err(io::Error::other)?;
    if config.long_term_path.join(tail) != job.destination
        || legacy_stream(&config.medium_term_path, &job.source).as_deref() != Some(&job.stream)
    {
        return Err(io::Error::other(
            "recording relocation no longer matches configured roots",
        ));
    }
    let staged = job.destination.with_file_name(staged_name(job.staged));
    // The UUID was reserved by the catalog before even one media byte could be written.
    remove_stage(&staged)?;
    if !job.source.try_exists()? {
        if !job.destination_published && job.destination.try_exists()? {
            return Err(io::Error::other(
                "source disappeared before recording handoff",
            ));
        }
        return catalog.retire_relocation(job).map_err(io::Error::other);
    }
    verify_source(job)?;
    if same_entry(&job.source, &job.destination)? {
        return catalog.retire_relocation(job).map_err(io::Error::other);
    }
    if catalog
        .reader_leases()
        .conflicts(&job.id, &job.source.to_string_lossy())
        .map_err(io::Error::other)?
    {
        return Err(io::Error::other(
            "active recording reader defers relocation",
        ));
    }
    stage_source(&job.source, &job.destination, job.staged)?;
    sync_parent(&job.destination)?;
    catalog
        .update_recording_path(&job.id, &job.destination, true)
        .map_err(io::Error::other)?;
    if same_entry(&job.source, &job.destination)? {
        return Err(io::Error::other(
            "recording directories became aliases during publication",
        ));
    }
    verify_source(job)?;
    fs::remove_file(&job.source)?;
    sync_parent(&job.source)?;
    catalog.retire_relocation(job).map_err(io::Error::other)
}

fn verify_source(job: &Move) -> io::Result<()> {
    let meta = fs::symlink_metadata(&job.source)?;
    if !meta.is_file()
        || crate::storage::catalog::recording_file_identity(&job.source, &meta).as_deref()
            != Some(&job.source_identity)
    {
        return Err(io::Error::other(
            "recording source changed during relocation",
        ));
    }
    Ok(())
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing recording directory"))?;
    let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    crate::storage::long_term::inspection::removal::sync_directory(&directory)
}
