//! Runs explicit archive eviction and staged restore outside recording and control loops.

use super::{
    VerifiedReader,
    filesystem::FilesystemArchiveTarget,
    target::{ArchiveTarget, ObjectId, Version, Work},
};
use crate::storage::{
    catalog::{
        RecordingCatalogHandle,
        archive::{Job, Reply, Request},
        archive_playback::ArchiveRead,
    },
    volumes::root::Root,
};
use sha2::{Digest, Sha256};
use std::{io::Write, sync::Arc};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Verified,
    Unchanged,
}

pub(super) fn execute(
    catalog: &RecordingCatalogHandle,
    job: &Job,
    work: &Work<'_>,
) -> anyhow::Result<Outcome> {
    match job.action.as_str() {
        "evict" => evict(catalog, job, work),
        "restore" => restore(catalog, job, work),
        _ => anyhow::bail!("archive action is unsupported"),
    }
}

fn descriptor(job: &Job) -> anyhow::Result<ArchiveRead> {
    Ok(ArchiveRead {
        target_id: job.binding.id.clone(),
        root: job.binding.root.clone(),
        filesystem: job.binding.identity.filesystem.clone(),
        root_identity: job.binding.identity.directory.clone(),
        object_id: job.object_id.clone(),
        version: job
            .version
            .clone()
            .ok_or_else(|| anyhow::anyhow!("archive is not verified"))?,
        bytes: job.bytes,
        digest: job.digest,
        chunk_hashes: Arc::from(job.chunk_hashes.clone()),
    })
}

fn source_root(job: &Job) -> anyhow::Result<Root> {
    let root = Root::open(&job.source_root)?;
    anyhow::ensure!(
        root.identity() == &job.source_root_identity,
        "primary root identity changed"
    );
    Ok(root)
}

fn evict(catalog: &RecordingCatalogHandle, job: &Job, work: &Work<'_>) -> anyhow::Result<Outcome> {
    work.check()?;
    let operation = job
        .source_retirement_operation
        .clone()
        .unwrap_or_else(|| ObjectId::random().to_string());
    let root = source_root(job)?;
    let outcome = if job.local_evicted == 1 {
        Outcome::Unchanged
    } else {
        let path = if job.local_evicted == 2 {
            descriptor(job)?.path().to_string_lossy().into_owned()
        } else {
            job.source_path.clone()
        };
        let _lease = catalog.lease_archive_source(&job.recording_id, &path)?;
        verify_archive(job, work)?;
        drop(_lease);
        let Reply::Eviction(intent) = catalog.archive(Request::BeginEviction {
            recording_id: job.recording_id.clone(),
            operation: operation.clone(),
        })?
        else {
            anyhow::bail!("invalid archive eviction reply");
        };
        work.check()?;
        anyhow::ensure!(
            intent.source_identity == job.source_identity,
            "archive eviction source changed"
        );
        anyhow::ensure!(
            intent.source_bytes == job.bytes,
            "archive eviction source changed"
        );
        anyhow::ensure!(
            intent.archive.digest == job.digest,
            "archive eviction source changed"
        );
        root.retire_owned_until(
            &job.source_relative_key,
            &job.source_identity,
            job.bytes,
            job.digest,
            &operation,
            work.deadline,
        )?;
        work.check()?;
        catalog.archive(Request::FinishEviction {
            recording_id: job.recording_id.clone(),
            operation: operation.clone(),
        })?;
        Outcome::Verified
    };
    root.acknowledge_retirement(
        &job.source_relative_key,
        &job.source_identity,
        job.bytes,
        job.digest,
        &operation,
    )?;
    work.check()?;
    Ok(outcome)
}

fn verify_archive(job: &Job, work: &Work<'_>) -> anyhow::Result<()> {
    let target = FilesystemArchiveTarget::open(&job.binding.root, &job.binding.identity)?;
    let version = Version::new(
        ObjectId::parse(&job.object_id)?,
        job.version
            .clone()
            .ok_or_else(|| anyhow::anyhow!("archive is not ready"))?,
        job.bytes,
    )?;
    verify_version(&target, &version, job.digest, work)
}

fn restore(
    catalog: &RecordingCatalogHandle,
    job: &Job,
    work: &Work<'_>,
) -> anyhow::Result<Outcome> {
    work.check()?;
    if job.local_evicted == 0 {
        return Ok(Outcome::Unchanged);
    }
    anyhow::ensure!(
        job.local_evicted == 1,
        "primary eviction must complete before restore"
    );
    let root = source_root(job)?;
    let capacity = root.capacity(catalog.volume_ledger_revision()?)?;
    let Reply::Job(Some(intent)) = catalog.archive(Request::BeginRestore {
        id: job.id.clone(),
        capacity,
    })?
    else {
        anyhow::bail!("invalid archive restore reply");
    };
    let archive = descriptor(&intent)?;
    let lease =
        catalog.lease_archive_source(&intent.recording_id, &archive.path().to_string_lossy())?;
    let mut input = VerifiedReader::new(
        archive,
        Work {
            deadline: work.deadline,
            cancelled: work.cancelled,
        },
    )?;
    let target = FilesystemArchiveTarget::open(&intent.source_root, &intent.source_root_identity)?;
    let object = ObjectId::parse(
        intent
            .source_relative_key
            .strip_suffix(".mp4")
            .ok_or_else(|| anyhow::anyhow!("invalid restore destination"))?,
    )?;
    let mut checkpoint = |version: &Version| {
        catalog.archive(Request::RestoreStage {
            id: intent.id.clone(),
            version: version.clone(),
        })?;
        Ok(())
    };
    let version = target.publish(
        object,
        &mut input,
        intent.bytes,
        intent.restore_stage.as_ref(),
        &mut checkpoint,
        work,
    )?;
    verify_version(&target, &version, intent.digest, work)?;
    drop(input);
    drop(lease);
    work.check()?;
    catalog.archive(Request::RestoreReady {
        id: intent.id,
        version,
        digest: intent.digest,
    })?;
    Ok(Outcome::Verified)
}

fn verify_version(
    target: &impl ArchiveTarget,
    version: &Version,
    digest: [u8; 32],
    work: &Work<'_>,
) -> anyhow::Result<()> {
    let mut hash = HashSink::default();
    target.read(version, &mut hash, work)?;
    anyhow::ensure!(
        hash.bytes == version.bytes && <[u8; 32]>::from(hash.hash.finalize()) == digest,
        "archive bytes failed independent verification"
    );
    Ok(())
}

#[derive(Default)]
struct HashSink {
    hash: Sha256,
    bytes: u64,
}

impl Write for HashSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("archive hash length overflow"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
