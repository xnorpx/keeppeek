//! Retires unused images through the existing confined removal receipts.

use super::{Manager, Publication, Reply, Request};

impl Manager {
    pub(crate) fn retire_unused_image(&self, id: &str) -> anyhow::Result<bool> {
        let Reply::ImageRetirement(job) = self
            .inner
            .catalog
            .volume_location(Request::ImageRetirement(id.into()))?
        else {
            anyhow::bail!("invalid image retirement reply");
        };
        let Some(job) = job else { return Ok(false) };
        if job.acknowledged {
            return Ok(true);
        }
        let _worker = self.inner.catalog.claim_volume_move(&job.operation)?;
        let location = &job.location;
        let index = self
            .inner
            .configuration
            .volumes
            .iter()
            .position(|volume| volume.id.as_str() == location.volume)
            .ok_or_else(|| anyhow::anyhow!("image volume is not configured"))?;
        anyhow::ensure!(location.generation == 1, "image volume generation changed");
        let path = self.inner.configuration.volumes[index]
            .root
            .join(&location.relative_key);
        if self
            .inner
            .catalog
            .reader_leases()
            .conflicts(&location.object.id, &path.to_string_lossy())?
        {
            return Ok(true);
        }
        let root = self.inner.writable_root(index)?;
        if !job.complete {
            root.retire_owned(
                &location.relative_key,
                &location.file_identity,
                location.bytes,
                location.digest,
                &job.operation,
            )?;
            self.inner
                .catalog
                .volume_location(Request::ImageRetired(Publication {
                    operation: job.operation.clone(),
                    bytes: location.bytes,
                    file_identity: location.file_identity.clone(),
                    digest: location.digest,
                }))?;
        }
        root.acknowledge_retirement(
            &location.relative_key,
            &location.file_identity,
            location.bytes,
            location.digest,
            &job.operation,
        )?;
        self.inner
            .catalog
            .volume_location(Request::ImageRetirementAcknowledged(job.operation))?;
        Ok(true)
    }
}
