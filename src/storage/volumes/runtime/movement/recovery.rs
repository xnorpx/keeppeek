use super::*;
use crate::storage::catalog::locations::{Publication, moves::Job};

impl Manager {
    /// Resumes one journaled move without removing either copy.
    ///
    /// # Errors
    /// Rejects concurrent workers, cancellation, ambiguous ownership, or changed file evidence.
    pub fn resume_move(&self, job_id: &str, cancelled: impl Fn() -> bool) -> anyhow::Result<()> {
        let _move_lease = self.inner.catalog.claim_volume_move(job_id)?;
        let deadline = Instant::now() + Duration::from_secs(300);
        check_work(deadline, &cancelled)?;
        uuid::Uuid::parse_str(job_id)?;
        let Reply::Move(job) = self
            .inner
            .catalog
            .volume_location(Request::Move(job_id.to_owned()))?
        else {
            anyhow::bail!("invalid move journal reply");
        };
        anyhow::ensure!(!job.cancellation_requested, "move cancellation is pending");
        match job.phase.as_str() {
            "reserved" => self.resume_reserved(&job, deadline, &cancelled),
            "verified" | "file_published" => self.resume_publication(&job, deadline, &cancelled),
            "published" | "complete" => self.verify_terminal(&job),
            _ => anyhow::bail!("move phase requires retirement or cancellation recovery"),
        }
    }

    fn move_volume(&self, volume: &str, generation: u64) -> anyhow::Result<usize> {
        anyhow::ensure!(generation == 1, "move volume generation is unsupported");
        self.inner
            .configuration
            .volumes
            .iter()
            .position(|entry| entry.id.as_str() == volume)
            .ok_or_else(|| anyhow::anyhow!("move volume is not configured"))
    }

    fn resume_reserved(
        &self,
        job: &Job,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
    ) -> anyhow::Result<()> {
        let index = self.move_volume(&job.destination.volume, job.destination.generation)?;
        let root = self.inner.writable_root(index)?;
        let temporary = format!("{}.tmp", job.id);
        let input = self.open_owned(&job.source)?;
        check_work(deadline, cancelled)?;
        let file = match &job.destination.file_identity {
            Some(identity) => root.open_owned_writable(
                &temporary,
                identity,
                job.destination.materialized_bytes,
                job.destination.bytes,
            )?,
            None => root.create_file(&temporary)?,
        };
        let reservation = Reservation {
            inner: Arc::clone(&self.inner),
            index,
            operation: job.destination_operation.clone(),
            key: job.destination.relative_key.clone(),
            path: self.inner.configuration.volumes[index]
                .root
                .join(&job.destination.relative_key),
            bytes: job.destination.bytes,
            _writer_lease: None,
        };
        let mut output = ReservedFile {
            reservation,
            file,
            evidence: RefCell::new(None),
            published: Cell::new(false),
            failed: false,
        };
        output.checkpoint()?;
        self.finish_copy(&job.source, input, output, deadline, cancelled)
    }

    fn resume_publication(
        &self,
        job: &Job,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
    ) -> anyhow::Result<()> {
        let index = self.move_volume(&job.destination.volume, job.destination.generation)?;
        let root = self.inner.writable_root(index)?;
        let evidence = destination_evidence(job)?;
        if job.phase == "verified" {
            check_work(deadline, cancelled)?;
            match root.open_owned_writable(
                &format!("{}.tmp", job.id),
                &evidence.file_identity,
                evidence.bytes,
                evidence.bytes,
            ) {
                Ok(mut temporary) => {
                    let observed = temporary.evidence()?;
                    anyhow::ensure!(
                        observed
                            == (
                                evidence.bytes,
                                evidence.file_identity.clone(),
                                evidence.digest
                            ),
                        "verified move temporary changed"
                    );
                    check_work(deadline, cancelled)?;
                    temporary.publish_staged(&job.destination.relative_key)?;
                }
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        let _published = verify_published(root, &job.destination.relative_key, &evidence)?;
        root.sync()?;
        check_work(deadline, cancelled)?;
        if job.phase == "verified" {
            self.advance_copy(Step::FilePublished(job.id.clone()), "file_published")?;
        }
        check_work(deadline, cancelled)?;
        self.advance_copy(Step::Publish(job.id.clone()), "published")
    }

    fn verify_terminal(&self, job: &Job) -> anyhow::Result<()> {
        let Reply::Location(Some(current)) = self
            .inner
            .catalog
            .volume_location(Request::Lookup(job.object.clone()))?
        else {
            anyhow::bail!("completed move has no authoritative location");
        };
        anyhow::ensure!(
            current.revision > job.source.revision
                && current.bytes == job.source.bytes
                && current.digest == job.source.digest,
            "completed move authority changed"
        );
        if job.phase == "published" {
            anyhow::ensure!(
                current.volume == job.destination.volume
                    && current.generation == job.destination.generation
                    && current.relative_key == job.destination.relative_key
                    && Some(&current.file_identity) == job.destination.file_identity.as_ref(),
                "published move destination changed"
            );
        }
        let index = self.move_volume(&current.volume, current.generation)?;
        let evidence = Publication {
            operation: job.id.clone(),
            bytes: current.bytes,
            file_identity: current.file_identity,
            digest: current.digest,
        };
        let _file = verify_published(self.inner.root(index)?, &current.relative_key, &evidence)?;
        Ok(())
    }
}

fn destination_evidence(job: &Job) -> anyhow::Result<Publication> {
    Ok(Publication {
        operation: job.destination_operation.clone(),
        bytes: job.source.bytes,
        file_identity: job
            .destination
            .file_identity
            .clone()
            .ok_or_else(|| anyhow::anyhow!("verified move identity is missing"))?,
        digest: job.source.digest,
    })
}
