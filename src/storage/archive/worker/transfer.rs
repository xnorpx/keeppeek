use super::*;
use crate::storage::{archive::target::Version, catalog::readers::LeaseSet};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Write};

struct DigestReader<'a> {
    source: &'a mut dyn Read,
    digest: Sha256,
    bytes: u64,
}

pub(super) struct Verification {
    pub digest: [u8; 32],
    pub chunk_hashes: Vec<u8>,
}

pub(super) struct VerificationSink {
    digest: Sha256,
    chunk_digest: Sha256,
    chunk_bytes: u64,
    bytes: u64,
    expected: u64,
    chunk_hashes: Vec<u8>,
}

pub(super) fn copy(
    catalog: &RecordingCatalogHandle,
    job: &Job,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    let _lease = source_lease(catalog, job)?;
    let root = Root::open(&job.source_root)?;
    anyhow::ensure!(
        root.identity() == &job.source_root_identity,
        "archive source root changed"
    );
    anyhow::ensure!(
        job.source_root.join(&job.source_relative_key) == std::path::Path::new(&job.source_path),
        "archive source location changed"
    );
    let mut source = root.open_owned(&job.source_relative_key, &job.source_identity, job.bytes)?;
    let modified = source.file_mut().metadata()?.modified()?;
    let target = FilesystemArchiveTarget::open(&job.binding.root, &job.binding.identity)?;
    let (version, verification) = publish_verify(catalog, job, &target, &mut source, work)?;
    source.revalidate()?;
    anyhow::ensure!(
        source.file_mut().metadata()?.modified()? == modified,
        "archive source changed during transfer"
    );
    ready(catalog, job, version, verification, work)
}

fn source_lease(catalog: &RecordingCatalogHandle, job: &Job) -> anyhow::Result<LeaseSet> {
    match job.kind.as_str() {
        "recording" => catalog.lease_archive_source(&job.recording_id, &job.source_path),
        "export" => {
            let (location, lease) = catalog
                .leased_export(&job.recording_id)?
                .ok_or_else(|| anyhow::anyhow!("archive export source is unavailable"))?;
            anyhow::ensure!(
                location.relative_key == job.source_relative_key
                    && location.file_identity == job.source_identity
                    && location.bytes == job.bytes
                    && location.digest == job.digest,
                "archive export source changed"
            );
            Ok(lease)
        }
        _ => anyhow::bail!("invalid archive source kind"),
    }
}

fn publish_verify(
    catalog: &RecordingCatalogHandle,
    job: &Job,
    target: &impl ArchiveTarget,
    source: &mut dyn Read,
    work: &Work<'_>,
) -> anyhow::Result<(Version, Verification)> {
    work.check()?;
    let root = Root::open(&job.binding.root)?;
    anyhow::ensure!(
        root.identity() == &job.binding.identity,
        "archive transfer root changed"
    );
    let capacity = root.capacity(catalog.volume_ledger_revision()?)?;
    catalog.archive(Request::ReserveTransfer {
        id: job.id.clone(),
        capacity,
    })?;
    let mut source = DigestReader {
        source,
        digest: Sha256::new(),
        bytes: 0,
    };
    let version = target.publish(
        ObjectId::parse(&job.object_id)?,
        &mut source,
        job.bytes,
        job.stage.as_ref(),
        &mut |version| {
            catalog.archive(Request::Stage {
                id: job.id.clone(),
                version: version.clone(),
            })?;
            Ok(())
        },
        work,
    )?;
    if source.bytes == 0 {
        hash_recovered_source(&mut source, work)?;
    }
    anyhow::ensure!(source.bytes == job.bytes, "archive source length changed");
    let digest: [u8; 32] = source.digest.finalize().into();
    anyhow::ensure!(digest == job.digest, "archive source checksum changed");
    let mut sink = VerificationSink::new(job.bytes)?;
    target.read(&version, &mut sink, work)?;
    let verification = sink.finish()?;
    anyhow::ensure!(
        verification.digest == job.digest,
        "archive readback checksum mismatch"
    );
    work.check()?;
    Ok((version, verification))
}

fn hash_recovered_source(source: &mut DigestReader<'_>, work: &Work<'_>) -> anyhow::Result<()> {
    let mut bytes = [0_u8; 65_536];
    loop {
        work.check()?;
        if source.read(&mut bytes)? == 0 {
            return work.check();
        }
    }
}

fn ready(
    catalog: &RecordingCatalogHandle,
    job: &Job,
    version: Version,
    verification: Verification,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    work.check()?;
    catalog.archive(Request::Ready {
        id: job.id.clone(),
        version,
        digest: verification.digest,
        chunk_hashes: verification.chunk_hashes,
    })?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ProbeOutcome {
    Verified,
    Unchanged,
}

pub(super) fn probe(
    catalog: &RecordingCatalogHandle,
    job: &Job,
    maximum_attempts: u32,
    work: &Work<'_>,
) -> anyhow::Result<ProbeOutcome> {
    let target = FilesystemArchiveTarget::open(&job.binding.root, &job.binding.identity)?;
    work.check()?;
    let Some(mut current) = current_probe(catalog, job)? else {
        work.check()?;
        return Ok(ProbeOutcome::Unchanged);
    };
    work.check()?;
    if matches!(current.state.as_str(), "queued" | "copying" | "failed") {
        let Reply::Job(Some(started)) = catalog.archive(Request::Start {
            id: job.id.clone(),
            maximum_attempts,
        })?
        else {
            anyhow::bail!("invalid archive probe start reply");
        };
        let (version, verification) = publish_verify(
            catalog,
            &started,
            &target,
            &mut Cursor::new(super::super::PROBE_PAYLOAD),
            work,
        )?;
        ready(catalog, &started, version, verification, work)?;
        current = catalog.archive_job(&job.id)?;
    }
    let version = Version::new(
        ObjectId::parse(&current.object_id)?,
        current
            .version
            .ok_or_else(|| anyhow::anyhow!("probe version is unavailable"))?,
        current.bytes,
    )?;
    let operation = ObjectId::parse(&job.id)?;
    if current.state != "deleted" {
        target.delete(&version, current.digest, operation, work)?;
        catalog.archive(Request::ProbeDeleted(job.id.clone()))?;
    }
    target.acknowledge_delete(&version, current.digest, operation, work)?;
    catalog.archive(Request::ProbeAcknowledged(job.id.clone()))?;
    Ok(ProbeOutcome::Verified)
}

fn current_probe(catalog: &RecordingCatalogHandle, job: &Job) -> anyhow::Result<Option<Job>> {
    // A queued snapshot can outlive a manual probe and the later pruning of its completed row.
    let Reply::Job(current) = catalog.archive(Request::Get(job.id.clone()))? else {
        anyhow::bail!("invalid archive probe lookup reply");
    };
    let Some(current) = current else {
        return Ok(None);
    };
    anyhow::ensure!(
        current.kind == "probe" && current.binding == job.binding,
        "archive probe intent changed"
    );
    if current.cancelled {
        return Ok(None);
    }
    if current.probe_acknowledged {
        anyhow::ensure!(
            current.state == "deleted",
            "archive probe acknowledgement changed"
        );
        return Ok(None);
    }
    Ok(Some(current))
}

impl Read for DigestReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let read = self.source.read(bytes)?;
        self.digest.update(&bytes[..read]);
        self.bytes = self
            .bytes
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("archive source byte count overflow"))?;
        Ok(read)
    }
}

impl VerificationSink {
    pub(super) fn new(expected: u64) -> anyhow::Result<Self> {
        anyhow::ensure!(
            expected > 0 && expected <= 64 * 1024 * 1024 * 1024,
            "invalid archive verification size"
        );
        let hashes = usize::try_from(
            expected
                .div_ceil(super::super::CHUNK_BYTES)
                .checked_mul(32)
                .ok_or_else(|| anyhow::anyhow!("archive chunk count overflow"))?,
        )?;
        Ok(Self {
            digest: Sha256::new(),
            chunk_digest: Sha256::new(),
            chunk_bytes: 0,
            bytes: 0,
            expected,
            chunk_hashes: Vec::with_capacity(hashes),
        })
    }

    pub(super) fn finish(mut self) -> anyhow::Result<Verification> {
        anyhow::ensure!(
            self.bytes == self.expected,
            "archive verification length mismatch"
        );
        if self.chunk_bytes != 0 {
            self.chunk_hashes
                .extend_from_slice(&self.chunk_digest.finalize());
        }
        Ok(Verification {
            digest: self.digest.finalize().into(),
            chunk_hashes: self.chunk_hashes,
        })
    }
}

impl Write for VerificationSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= self.expected)
            .ok_or_else(|| std::io::Error::other("archive verification exceeds its bound"))?;
        self.digest.update(bytes);
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let amount = remaining.len().min(
                usize::try_from(super::super::CHUNK_BYTES - self.chunk_bytes)
                    .map_err(std::io::Error::other)?,
            );
            self.chunk_digest.update(&remaining[..amount]);
            self.chunk_bytes += amount as u64;
            remaining = &remaining[amount..];
            if self.chunk_bytes == super::super::CHUNK_BYTES {
                self.chunk_hashes
                    .extend_from_slice(&std::mem::take(&mut self.chunk_digest).finalize());
                self.chunk_bytes = 0;
            }
        }
        self.bytes = total;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
