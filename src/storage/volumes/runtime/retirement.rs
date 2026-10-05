use super::{Manager, Publication, Reply, Request};
use crate::storage::catalog::locations::moves::{Job, Step};
use crate::storage::volumes::root::OwnedFile;

#[cfg(test)]
mod tests;

impl Manager {
    /// Retires a verified old copy after publication and after its readers finish.
    /// Returns false while a reader still holds the object.
    ///
    /// # Errors
    /// Preserves ownership when authority, identity, or durable removal cannot be proved.
    pub fn retire_move(&self, job_id: &str) -> anyhow::Result<bool> {
        let _lease = self.inner.catalog.claim_volume_move(job_id)?;
        let Reply::Move(job) = self
            .inner
            .catalog
            .volume_location(Request::Move(job_id.into()))?
        else {
            anyhow::bail!("invalid retirement journal reply");
        };
        if job.phase == "complete" {
            self.acknowledge_retired_move(&job)?;
            return Ok(true);
        }
        if !self.retirement_ready(job_id)? {
            return Ok(false);
        }
        let _destination = self.pin_retirement_destination(&job)?;
        let root = self.owned_root(&job.source, true)?;
        self.inner
            .catalog
            .volume_location(Request::AdvanceMove(Step::Retiring(job_id.into())))?;
        let retire = if job.source.volume.starts_with("legacy-") {
            super::Root::retire_legacy
        } else {
            super::Root::retire_owned
        };
        retire(
            &root,
            &job.source.relative_key,
            &job.source.file_identity,
            job.source.bytes,
            job.source.digest,
            job_id,
        )?;
        self.inner
            .catalog
            .volume_location(Request::AdvanceMove(Step::Retired(Publication {
                operation: job_id.into(),
                bytes: job.source.bytes,
                file_identity: job.source.file_identity.clone(),
                digest: job.source.digest,
            })))?;
        self.acknowledge_retired_move(&job)?;
        Ok(true)
    }

    fn acknowledge_retired_move(&self, job: &Job) -> anyhow::Result<()> {
        if job.receipt_acknowledged {
            return Ok(());
        }
        let root = self.owned_root(&job.source, true)?;
        let acknowledge = if job.source.volume.starts_with("legacy-") {
            super::Root::acknowledge_legacy
        } else {
            super::Root::acknowledge_retirement
        };
        acknowledge(
            &root,
            &job.source.relative_key,
            &job.source.file_identity,
            job.source.bytes,
            job.source.digest,
            &job.id,
        )?;
        self.inner
            .catalog
            .volume_location(Request::AdvanceMove(Step::Acknowledged(job.id.clone())))?;
        Ok(())
    }

    fn pin_retirement_destination(&self, job: &Job) -> anyhow::Result<OwnedFile> {
        let Reply::Location(Some(current)) = self
            .inner
            .catalog
            .volume_location(Request::Lookup(job.object.clone()))?
        else {
            anyhow::bail!("retirement destination has no authority");
        };
        anyhow::ensure!(
            current.volume == job.destination.volume
                && current.generation == job.destination.generation
                && current.relative_key == job.destination.relative_key
                && Some(&current.file_identity) == job.destination.file_identity.as_ref()
                && current.bytes == job.source.bytes
                && current.digest == job.source.digest
                && job.source.revision.checked_add(1) == Some(current.revision),
            "retirement destination authority changed"
        );
        let index = self
            .inner
            .configuration
            .volumes
            .iter()
            .position(|volume| volume.id.as_str() == current.volume)
            .ok_or_else(|| anyhow::anyhow!("destination volume is not configured"))?;
        anyhow::ensure!(
            current.generation == 1,
            "destination volume generation changed"
        );
        let mut file = self.inner.root(index)?.open_owned(
            &current.relative_key,
            &current.file_identity,
            current.bytes,
        )?;
        let (bytes, identity, digest) = file.inspect_evidence()?;
        anyhow::ensure!(
            bytes == current.bytes && identity == current.file_identity && digest == current.digest,
            "retirement destination changed"
        );
        Ok(file)
    }
}
