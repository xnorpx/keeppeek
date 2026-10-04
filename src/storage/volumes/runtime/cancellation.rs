use super::{Manager, OwnedFile, Reply, Request, Root};
use crate::storage::catalog::locations::moves::{Cancellation, Job, Step};

#[cfg(test)]
mod tests;

impl Manager {
    /// Durably cancels a pending move and removes only its proven target copy.
    ///
    /// # Errors
    /// A busy worker leaves cancellation latched for retry. Ambiguous files are preserved.
    pub fn cancel_move(&self, job_id: &str) -> anyhow::Result<()> {
        let Reply::Move(job) = self
            .inner
            .catalog
            .volume_location(Request::Move(job_id.into()))?
        else {
            anyhow::bail!("invalid cancellation journal reply");
        };
        if !job.cancellation_requested {
            self.inner
                .catalog
                .volume_location(Request::AdvanceMove(Step::Cancel(job_id.into())))?;
        }
        self.finish_cancelled_move(job_id)
    }

    pub(super) fn finish_cancelled_move(&self, job_id: &str) -> anyhow::Result<()> {
        let _lease = self.inner.catalog.claim_volume_move(job_id)?;
        let Reply::Move(job) = self
            .inner
            .catalog
            .volume_location(Request::Move(job_id.into()))?
        else {
            anyhow::bail!("invalid cancellation journal reply");
        };
        anyhow::ensure!(
            job.cancellation_requested,
            "move cancellation is not requested"
        );
        anyhow::ensure!(
            matches!(
                job.phase.as_str(),
                "reserved" | "verified" | "file_published" | "cancelled"
            ),
            "published move cannot be cancelled"
        );
        if job.phase == "cancelled" && job.receipt_acknowledged {
            return Ok(());
        }
        let _source = if job.phase == "cancelled" {
            None
        } else {
            Some(self.pin_cancellation_source(&job)?)
        };
        let root = self
            .inner
            .root(self.cancellation_volume(&job.destination.volume, job.destination.generation)?)?;
        let evidence = match &job.cancellation {
            Some(evidence) => evidence.clone(),
            None => {
                let evidence = capture_target(root, &job)?;
                self.inner.catalog.volume_location(Request::AdvanceMove(
                    Step::CancellationVerified {
                        id: job.id.clone(),
                        evidence: evidence.clone(),
                    },
                ))?;
                evidence
            }
        };
        self.remove_cancelled_target(root, &job, &evidence)
    }

    fn remove_cancelled_target(
        &self,
        root: &Root,
        job: &Job,
        evidence: &Cancellation,
    ) -> anyhow::Result<()> {
        // ponytail: use the existing durable retirement receipt for cancellation too.
        if job.phase != "cancelled" {
            match evidence {
                Cancellation::Empty => root
                    .confirm_absent(&[&format!("{}.tmp", job.id), &job.destination.relative_key])?,
                Cancellation::File {
                    relative_key,
                    bytes,
                    file_identity,
                    digest,
                } => {
                    root.retire_owned(relative_key, file_identity, *bytes, *digest, &job.id)?;
                }
            }
            self.inner
                .catalog
                .volume_location(Request::AdvanceMove(Step::Cancelled(job.id.clone())))?;
        }
        if let Cancellation::File {
            relative_key,
            bytes,
            file_identity,
            digest,
        } = evidence
        {
            root.acknowledge_retirement(relative_key, file_identity, *bytes, *digest, &job.id)?;
        }
        self.inner
            .catalog
            .volume_location(Request::AdvanceMove(Step::Acknowledged(job.id.clone())))?;
        Ok(())
    }

    fn cancellation_volume(&self, volume: &str, generation: u64) -> anyhow::Result<usize> {
        anyhow::ensure!(generation == 1, "cancellation volume generation changed");
        self.inner
            .configuration
            .volumes
            .iter()
            .position(|entry| entry.id.as_str() == volume)
            .ok_or_else(|| anyhow::anyhow!("cancellation volume is not configured"))
    }

    fn pin_cancellation_source(&self, job: &Job) -> anyhow::Result<OwnedFile> {
        let Reply::Location(Some(current)) = self
            .inner
            .catalog
            .volume_location(Request::Lookup(job.object.clone()))?
        else {
            anyhow::bail!("cancelled move has no authoritative source");
        };
        anyhow::ensure!(
            current == job.source,
            "cancelled move source authority changed"
        );
        let index = self.cancellation_volume(&current.volume, current.generation)?;
        let mut file = self.inner.root(index)?.open_owned(
            &current.relative_key,
            &current.file_identity,
            current.bytes,
        )?;
        anyhow::ensure!(
            file.inspect_evidence()? == (current.bytes, current.file_identity, current.digest),
            "cancelled move source changed"
        );
        Ok(file)
    }
}

fn capture_target(root: &Root, job: &Job) -> anyhow::Result<Cancellation> {
    let temporary = format!("{}.tmp", job.id);
    let Some(identity) = &job.destination.file_identity else {
        anyhow::ensure!(
            job.phase == "reserved" && job.destination.materialized_bytes == 0,
            "move target ownership is missing"
        );
        root.confirm_absent(&[&temporary, &job.destination.relative_key])?;
        return Ok(Cancellation::Empty);
    };
    let (key, mut file) = open_target(root, job, &temporary, identity)?;
    let (bytes, file_identity, digest) = if job.phase == "reserved" {
        file.evidence()?
    } else {
        file.inspect_evidence()?
    };
    anyhow::ensure!(
        file_identity == *identity
            && bytes >= job.destination.materialized_bytes
            && bytes <= job.destination.bytes,
        "cancellation target evidence changed"
    );
    if job.phase != "reserved" {
        anyhow::ensure!(
            bytes == job.source.bytes && digest == job.source.digest,
            "verified cancellation target changed"
        );
    }
    Ok(Cancellation::File {
        relative_key: key,
        bytes,
        file_identity,
        digest,
    })
}

fn open_target(
    root: &Root,
    job: &Job,
    temporary: &str,
    identity: &str,
) -> anyhow::Result<(String, OwnedFile)> {
    let target = if job.phase == "reserved" {
        (
            temporary.to_owned(),
            root.open_owned_writable(
                temporary,
                identity,
                job.destination.materialized_bytes,
                job.destination.bytes,
            )?,
        )
    } else if job.phase == "verified" {
        match root.open_owned(temporary, identity, job.source.bytes) {
            Ok(file) => (temporary.to_owned(), file),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                (
                    job.destination.relative_key.clone(),
                    root.open_owned(&job.destination.relative_key, identity, job.source.bytes)?,
                )
            }
            Err(error) => return Err(error),
        }
    } else {
        anyhow::ensure!(
            job.phase == "file_published",
            "cancellation evidence is missing"
        );
        (
            job.destination.relative_key.clone(),
            root.open_owned(&job.destination.relative_key, identity, job.source.bytes)?,
        )
    };
    Ok(target)
}
