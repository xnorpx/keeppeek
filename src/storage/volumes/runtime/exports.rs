//! Removes cancelled exports through the existing owned-file retirement receipts.

use super::{Manager, Reply, Request, Root};
use crate::storage::catalog::locations::{
    export_cleanup::{Action, Job, Owned},
    moves::Cancellation,
};

impl Manager {
    pub(crate) fn finish_export_retirement(&self, id: &str) -> anyhow::Result<bool> {
        if self.export_cleanup(id)?.is_none() {
            return Ok(false);
        }
        let _worker = self.inner.catalog.claim_volume_move(id)?;
        let job = self
            .export_cleanup(id)?
            .ok_or_else(|| anyhow::anyhow!("export cleanup disappeared"))?;
        if job.acknowledged {
            return Ok(true);
        }
        let Some(owned) = &job.allocation else {
            self.export_cleanup_action(Action::Verify(id.into(), Cancellation::Empty))?;
            self.export_cleanup_action(Action::Complete(id.into()))?;
            self.export_cleanup_action(Action::Acknowledge(id.into()))?;
            return Ok(true);
        };
        let root = self.volume_root(&owned.volume, owned.generation, true)?;
        let path = root.path().join(&owned.relative_key);
        if self
            .inner
            .catalog
            .reader_leases()
            .conflicts(id, &path.to_string_lossy())?
        {
            return Ok(true);
        }
        let evidence = match job.evidence {
            Some(evidence) => evidence,
            None => {
                let evidence = capture(&root, owned)?;
                self.export_cleanup_action(Action::Verify(id.into(), evidence.clone()))?;
                evidence
            }
        };
        self.remove_export(&root, owned, &evidence, id, job.complete)?;
        Ok(true)
    }

    fn remove_export(
        &self,
        root: &Root,
        owned: &Owned,
        evidence: &Cancellation,
        id: &str,
        complete: bool,
    ) -> anyhow::Result<()> {
        // ponytail: Export cleanup shares the mover's receipt and periodic retry loop.
        if !complete {
            match &evidence {
                Cancellation::Empty => root.confirm_absent(&[&owned.relative_key])?,
                Cancellation::File {
                    relative_key,
                    bytes,
                    file_identity,
                    digest,
                } => {
                    let retire = if owned.volume.starts_with("legacy-") {
                        Root::retire_legacy
                    } else {
                        Root::retire_owned
                    };
                    retire(
                        root,
                        relative_key,
                        file_identity,
                        *bytes,
                        *digest,
                        &owned.operation,
                    )?;
                }
            }
            self.export_cleanup_action(Action::Complete(id.into()))?;
        }
        if let Cancellation::File {
            relative_key,
            bytes,
            file_identity,
            digest,
        } = evidence
        {
            let acknowledge = if owned.volume.starts_with("legacy-") {
                Root::acknowledge_legacy
            } else {
                Root::acknowledge_retirement
            };
            acknowledge(
                root,
                relative_key,
                file_identity,
                *bytes,
                *digest,
                &owned.operation,
            )?;
        }
        self.export_cleanup_action(Action::Acknowledge(id.into()))?;
        Ok(())
    }

    fn export_cleanup(&self, id: &str) -> anyhow::Result<Option<Box<Job>>> {
        let Reply::ExportCleanup(job) = self
            .inner
            .catalog
            .volume_location(Request::ExportCleanup(Action::Load(id.into())))?
        else {
            anyhow::bail!("invalid export cleanup reply");
        };
        Ok(job)
    }

    fn export_cleanup_action(&self, action: Action) -> anyhow::Result<()> {
        self.inner
            .catalog
            .volume_location(Request::ExportCleanup(action))?;
        Ok(())
    }
}

fn capture(root: &Root, owned: &Owned) -> anyhow::Result<Cancellation> {
    let Some(identity) = &owned.file_identity else {
        root.confirm_absent(&[&owned.relative_key])?;
        return Ok(Cancellation::Empty);
    };
    let mut file = if owned.volume.starts_with("legacy-") {
        root.open_legacy_owned(&owned.relative_key, identity, owned.bytes)?
    } else {
        root.open_owned_writable(
            &owned.relative_key,
            identity,
            owned.materialized_bytes,
            owned.bytes,
        )?
    };
    let (bytes, file_identity, digest) = if owned.volume.starts_with("legacy-") {
        file.inspect_evidence()?
    } else {
        file.evidence()?
    };
    if let Some(expected) = owned.digest {
        anyhow::ensure!(
            bytes == owned.bytes && digest == expected,
            "published export changed"
        );
    }
    Ok(Cancellation::File {
        relative_key: owned.relative_key.clone(),
        bytes,
        file_identity,
        digest,
    })
}
