use super::*;
use crate::storage::{archive::target::Version, volumes::root::OwnedFile};
use sha2::{Digest, Sha256};
use std::io::Read;

struct Destination<'a> {
    target: FilesystemArchiveTarget,
    object: ObjectId,
    stage: Option<&'a Version>,
}

pub(super) fn execute(
    catalog: &RecordingCatalogHandle,
    job: &Job,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    work.check()?;
    if job.cleanup_acknowledged {
        return Ok(());
    }
    let destination = destination(job)?;
    if destination.stage.is_none() && job.cleanup_version.is_none() {
        destination
            .target
            .confirm_absent(destination.object, work)?;
        catalog.archive(Request::CleanupComplete(job.id.clone()))?;
        catalog.archive(Request::CleanupAcknowledged(job.id.clone()))?;
        return Ok(());
    }
    let (version, digest, key) = match &job.cleanup_version {
        Some(version) => (
            version.clone(),
            job.cleanup_digest
                .ok_or_else(|| anyhow::anyhow!("archive cleanup digest is unavailable"))?,
            job.cleanup_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("archive cleanup key is unavailable"))?,
        ),
        None => capture(catalog, job, &destination, work)?,
    };
    let operation = ObjectId::parse(&job.id)?;
    if !job.cleanup_complete {
        destination
            .target
            .delete_key(&key, &version, digest, operation, work)?;
        catalog.archive(Request::CleanupComplete(job.id.clone()))?;
    }
    destination
        .target
        .acknowledge_key(&key, &version, digest, operation, work)?;
    catalog.archive(Request::CleanupAcknowledged(job.id.clone()))?;
    Ok(())
}

fn destination(job: &Job) -> anyhow::Result<Destination<'_>> {
    if job.state == "ready" && job.action == "restore" {
        let name = job
            .source_relative_key
            .strip_suffix(".mp4")
            .ok_or_else(|| anyhow::anyhow!("archive restore destination is invalid"))?;
        Ok(Destination {
            target: FilesystemArchiveTarget::open(&job.source_root, &job.source_root_identity)?,
            object: ObjectId::parse(name)?,
            stage: job.restore_stage.as_ref(),
        })
    } else {
        Ok(Destination {
            target: FilesystemArchiveTarget::open(&job.binding.root, &job.binding.identity)?,
            object: ObjectId::parse(&job.object_id)?,
            stage: job.stage.as_ref(),
        })
    }
}

fn capture(
    catalog: &RecordingCatalogHandle,
    job: &Job,
    destination: &Destination<'_>,
    work: &Work<'_>,
) -> anyhow::Result<(Version, [u8; 32], String)> {
    let staged = destination
        .stage
        .ok_or_else(|| anyhow::anyhow!("archive staging evidence is unavailable"))?;
    let (mut file, key) = cleanup_file(job, &destination.target, staged, work)?;
    let bytes = file.file_mut().metadata()?.len();
    anyhow::ensure!(
        bytes <= job.bytes,
        "archive cleanup exceeds its reservation"
    );
    let version = Version::new(staged.object, staged.token.clone(), bytes)?;
    let digest = hash(&mut file, bytes, work)?;
    catalog.archive(Request::CleanupEvidence {
        id: job.id.clone(),
        version: version.clone(),
        digest,
        key: key.clone(),
    })?;
    Ok((version, digest, key))
}

fn cleanup_file(
    job: &Job,
    target: &FilesystemArchiveTarget,
    staged: &Version,
    work: &Work<'_>,
) -> anyhow::Result<(OwnedFile, String)> {
    if staged.bytes == job.bytes {
        match target.open_local(staged, work) {
            Ok(file) => return Ok((file, format!("{}.mp4", staged.object))),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    let file = target.open_stage(staged, job.bytes, work)?;
    Ok((file, format!("{}.tmp", staged.object)))
}

fn hash(file: &mut OwnedFile, bytes: u64, work: &Work<'_>) -> anyhow::Result<[u8; 32]> {
    let modified = file.file_mut().metadata()?.modified()?;
    let mut remaining = bytes;
    let mut buffer = [0_u8; 65_536];
    let mut digest = Sha256::new();
    while remaining != 0 {
        work.check()?;
        let amount = usize::try_from(remaining.min(buffer.len() as u64))?;
        file.read_exact(&mut buffer[..amount])?;
        digest.update(&buffer[..amount]);
        remaining -= amount as u64;
    }
    work.check()?;
    file.revalidate()?;
    anyhow::ensure!(
        file.file_mut().metadata()?.len() == bytes
            && file.file_mut().metadata()?.modified()? == modified,
        "archive cleanup object changed during verification"
    );
    Ok(digest.finalize().into())
}
