//! Recovers abandoned thumbnail reservations after their writer lease ends.

use super::{Manager, Publication, Reply, Request};

impl Manager {
    pub(super) fn recover_pending_image(&self, operation: &str) -> anyhow::Result<bool> {
        if self.pending_image(operation)?.is_none() {
            return Ok(false);
        }
        // ponytail: Reuse the existing worker lease instead of adding writer heartbeats.
        let _writer = self.inner.catalog.claim_volume_move(operation)?;
        let Some(owned) = self.pending_image(operation)? else {
            return Ok(true);
        };
        anyhow::ensure!(owned.generation == 1, "image volume generation changed");
        let index = self
            .inner
            .configuration
            .volumes
            .iter()
            .position(|volume| volume.id.as_str() == owned.volume)
            .ok_or_else(|| anyhow::anyhow!("image volume is not configured"))?;
        let root = self.inner.writable_root(index)?;
        let Some(identity) = &owned.file_identity else {
            root.confirm_absent(&[&owned.relative_key])?;
            self.inner
                .catalog
                .volume_location(Request::EmptyImageAbandoned(operation.into()))?;
            return Ok(true);
        };
        let mut file = root.open_owned_writable(
            &owned.relative_key,
            identity,
            owned.materialized_bytes,
            owned.bytes,
        )?;
        let (bytes, file_identity, digest) = file.evidence()?;
        self.inner
            .catalog
            .volume_location(Request::ImageAbandoned(Publication {
                operation: operation.into(),
                bytes,
                file_identity,
                digest,
            }))?;
        drop(file);
        drop(_writer);
        self.retire_unused_image(operation)?;
        Ok(true)
    }

    fn pending_image(
        &self,
        operation: &str,
    ) -> anyhow::Result<Option<Box<crate::storage::catalog::locations::export_cleanup::Owned>>>
    {
        let Reply::PendingImage(owned) = self
            .inner
            .catalog
            .volume_location(Request::PendingImage(operation.into()))?
        else {
            anyhow::bail!("invalid pending image reply");
        };
        Ok(owned)
    }
}
