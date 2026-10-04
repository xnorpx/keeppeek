use super::{Manager, Reservation, ReservedFile};
use crate::storage::catalog::locations::{Location, Reply, Request, moves::Step};
use crate::storage::volumes::root::{OwnedFile, Root};
use std::{
    cell::{Cell, RefCell},
    io::{Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};

mod recovery;
#[cfg(test)]
mod recovery_tests;

impl Manager {
    /// Copies a committed move reservation and publishes its verified destination.
    /// The source remains intact. Failures retain the journal and any created destination.
    ///
    /// # Errors
    /// Rejects changed intent, unsafe files, cancellation, expired work, or failed publication.
    /// Interrupted work requires journal recovery; this entry point starts only reserved jobs.
    pub(super) fn copy_move(
        &self,
        job_id: &str,
        source: &Location,
        destination: Reservation,
        cancelled: impl Fn() -> bool,
    ) -> anyhow::Result<()> {
        let _move_lease = self.inner.catalog.claim_volume_move(job_id)?;
        let deadline = Instant::now() + Duration::from_secs(300);
        check_work(deadline, &cancelled)?;
        self.validate_copy(job_id, source, &destination)?;
        let source_index = self
            .inner
            .configuration
            .volumes
            .iter()
            .position(|volume| volume.id.as_str() == source.volume)
            .ok_or_else(|| anyhow::anyhow!("move source volume is not configured"))?;
        let input = self.inner.root(source_index)?.open_owned(
            &source.relative_key,
            &source.file_identity,
            source.bytes,
        )?;
        let root = self.inner.writable_root(destination.index)?;
        let temporary = format!("{job_id}.tmp");
        let mut output = ReservedFile {
            file: root.create_file(&temporary)?,
            reservation: destination,
            evidence: RefCell::new(None),
            published: Cell::new(false),
            failed: false,
        };
        output.checkpoint()?;
        self.finish_copy(source, input, output, deadline, &cancelled)
    }

    fn finish_copy(
        &self,
        source: &Location,
        mut input: OwnedFile,
        mut output: ReservedFile,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
    ) -> anyhow::Result<()> {
        use std::io::{Seek, SeekFrom};
        let job_id = output.reservation.operation.clone();
        let root = self.inner.writable_root(output.reservation.index)?;
        let final_key = output.reservation.key.clone();
        let offset = output.file.file_mut().metadata()?.len();
        anyhow::ensure!(offset <= source.bytes, "move copy exceeds source length");
        input.file_mut().seek(SeekFrom::Start(offset))?;
        output.file.file_mut().seek(SeekFrom::Start(offset))?;
        copy_bytes(
            &mut input,
            &mut output,
            source.bytes - offset,
            deadline,
            cancelled,
        )?;
        let evidence = output.evidence()?;
        anyhow::ensure!(
            evidence.bytes == source.bytes && evidence.digest == source.digest,
            "move copy does not match the authoritative source"
        );
        check_work(deadline, cancelled)?;
        self.advance_copy(Step::Verified(evidence.clone()), "verified")?;
        check_work(deadline, cancelled)?;
        output.file.publish_staged(&final_key)?;
        let _published = verify_published(root, &final_key, &evidence)?;
        check_work(deadline, cancelled)?;
        self.advance_copy(Step::FilePublished(job_id.to_owned()), "file_published")?;
        check_work(deadline, cancelled)?;
        self.advance_copy(Step::Publish(job_id), "published")
    }

    fn validate_copy(
        &self,
        job_id: &str,
        source: &Location,
        destination: &Reservation,
    ) -> anyhow::Result<()> {
        uuid::Uuid::parse_str(job_id)?;
        anyhow::ensure!(
            Arc::ptr_eq(&self.inner, &destination.inner)
                && destination.operation == job_id
                && destination.bytes >= source.bytes,
            "move reservation does not belong to this job"
        );
        let Reply::Move(job) = self
            .inner
            .catalog
            .volume_location(Request::Move(job_id.to_owned()))?
        else {
            anyhow::bail!("invalid move journal reply");
        };
        anyhow::ensure!(
            job.phase == "reserved"
                && !job.cancellation_requested
                && job.source == *source
                && job.destination_operation == destination.operation
                && job.destination.volume
                    == self.inner.configuration.volumes[destination.index]
                        .id
                        .as_str()
                && job.destination.generation == 1
                && job.destination.relative_key == destination.key
                && job.destination.bytes == destination.bytes,
            "move source or journal changed"
        );
        Ok(())
    }

    fn advance_copy(&self, step: Step, phase: &str) -> anyhow::Result<()> {
        let Reply::Move(job) = self
            .inner
            .catalog
            .volume_location(Request::AdvanceMove(step))?
        else {
            anyhow::bail!("invalid move journal reply");
        };
        anyhow::ensure!(job.phase == phase, "move journal did not advance");
        Ok(())
    }
}

fn copy_bytes(
    input: &mut OwnedFile,
    output: &mut ReservedFile,
    bytes: u64,
    deadline: Instant,
    cancelled: &impl Fn() -> bool,
) -> anyhow::Result<()> {
    let mut remaining = bytes;
    let mut checkpoint_bytes = 0_u64;
    let mut last_checkpoint = Instant::now();
    // ponytail: Copy one 64 KiB block at a time; the shared deadline bounds the whole object.
    let mut buffer = [0_u8; 65_536];
    while remaining != 0 {
        check_work(deadline, cancelled)?;
        let amount = usize::try_from(remaining.min(buffer.len() as u64))?;
        input.read_exact(&mut buffer[..amount])?;
        output.write_all(&buffer[..amount])?;
        remaining -= amount as u64;
        checkpoint_bytes += amount as u64;
        if checkpoint_bytes >= 8 * 1_048_576
            || last_checkpoint.elapsed() >= Duration::from_millis(500)
        {
            output.checkpoint()?;
            checkpoint_bytes = 0;
            last_checkpoint = Instant::now();
        }
    }
    check_work(deadline, cancelled)
}

fn verify_published(
    root: &Root,
    key: &str,
    expected: &crate::storage::catalog::locations::Publication,
) -> anyhow::Result<OwnedFile> {
    let mut file = root.open_owned(key, &expected.file_identity, expected.bytes)?;
    let (bytes, identity, digest) = file.inspect_evidence()?;
    anyhow::ensure!(
        bytes == expected.bytes && identity == expected.file_identity && digest == expected.digest,
        "published move copy changed"
    );
    Ok(file)
}

fn check_work(deadline: Instant, cancelled: &impl Fn() -> bool) -> anyhow::Result<()> {
    anyhow::ensure!(!cancelled(), "move cancelled");
    anyhow::ensure!(Instant::now() < deadline, "move copy deadline expired");
    Ok(())
}
