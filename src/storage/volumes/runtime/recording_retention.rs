//! Completes journaled recording retention through pinned-root removal receipts.

use super::{Manager, Publication, Reply, Request, VolumeHealth, VolumeState};
use crate::storage::catalog::locations::recordings::{Action, Job, Reason};

impl Manager {
    pub(super) fn check_recording_pressure(&self) {
        // ponytail: Inspect at most 32 configured volumes in the existing periodic scan.
        for volume in &self.inner.configuration.volumes {
            if let Err(error) = self.queue_recording_retention(&volume.id, 0) {
                tracing::warn!(volume = %volume.id, %error, "volume pressure inspection deferred");
            }
        }
    }

    pub(super) fn request_pressure_retention(
        &self,
        rejected: &[super::super::RejectedVolume],
        required_bytes: u64,
    ) {
        use super::super::RejectionReason;
        for rejected in rejected {
            if !matches!(
                rejected.reason,
                RejectionReason::CapacityExceeded | RejectionReason::InsufficientSpace
            ) {
                continue;
            }
            if let Err(error) = self.queue_recording_retention(&rejected.id, required_bytes) {
                tracing::warn!(volume = %rejected.id, %error, "volume retention admission deferred");
            }
        }
    }

    pub(super) fn queue_recording_retention(
        &self,
        id: &super::super::VolumeId,
        required_bytes: u64,
    ) -> anyhow::Result<()> {
        let volume = self
            .inner
            .configuration
            .volumes
            .iter()
            .find(|volume| volume.id == *id)
            .ok_or_else(|| anyhow::anyhow!("retention volume is not configured"))?;
        if !matches!(volume.state, VolumeState::Enabled | VolumeState::Draining)
            || volume
                .capacity_bytes
                .is_some_and(|cap| required_bytes > cap)
        {
            return Ok(());
        }
        let observations = self.inner.observations(std::slice::from_ref(id))?;
        let observation = observations.first().expect("configured volume observation");
        if observation.health != VolumeHealth::Online {
            return Ok(());
        }
        let reason = if volume
            .capacity_bytes
            .is_some_and(|cap| observation.owned_bytes.saturating_add(required_bytes) > cap)
        {
            Reason::Capacity
        } else if observation.available_bytes
            < volume
                .minimum_free_bytes
                .max(volume.critical_free_bytes)
                .saturating_add(required_bytes)
        {
            Reason::DiskPressure
        } else {
            return Ok(());
        };
        self.admit_pressure_retention(id, reason)
    }

    fn admit_pressure_retention(
        &self,
        id: &super::super::VolumeId,
        reason: Reason,
    ) -> anyhow::Result<()> {
        let result = self
            .inner
            .catalog
            .volume_location(Request::RecordingRetention(Action::Begin {
                volume: id.to_string(),
                reason,
            }))?;
        let admitted = match result {
            Reply::RecordingRetirement(Some(_)) => true,
            Reply::RecordingRetirement(None) => matches!(
                self.inner
                    .catalog
                    .volume_location(Request::ImagePressure(id.to_string()))?,
                Reply::ImagePressure(true)
            ),
            _ => anyhow::bail!("invalid recording retention reply"),
        };
        if admitted {
            self.inner
                .rescan_requested
                .store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }

    pub(crate) fn finish_recording_retirement(&self, operation: &str) -> anyhow::Result<bool> {
        let Reply::RecordingRetirement(job) = self
            .inner
            .catalog
            .volume_location(Request::RecordingRetention(Action::Load(operation.into())))?
        else {
            anyhow::bail!("invalid recording retirement reply");
        };
        let Some(job) = job else {
            return Ok(false);
        };
        if job.acknowledged {
            return Ok(true);
        }
        let _worker = self.inner.catalog.claim_volume_move(operation)?;
        let location = &job.location;
        let root = self.owned_root(location, true)?;
        let path = root.path().join(&location.relative_key);
        if self
            .inner
            .catalog
            .reader_leases()
            .conflicts(&location.object.id, &path.to_string_lossy())?
        {
            return Ok(true);
        }
        self.remove_recording(&root, &job)?;
        Ok(true)
    }

    fn remove_recording(&self, root: &super::Root, job: &Job) -> anyhow::Result<()> {
        let location = &job.location;
        if !job.complete {
            let retire = if location.volume.starts_with("legacy-") {
                super::Root::retire_legacy
            } else {
                super::Root::retire_owned
            };
            retire(
                root,
                &location.relative_key,
                &location.file_identity,
                location.bytes,
                location.digest,
                &job.operation,
            )?;
            self.inner
                .catalog
                .volume_location(Request::RecordingRetention(Action::Complete(Publication {
                    operation: job.operation.clone(),
                    bytes: location.bytes,
                    file_identity: location.file_identity.clone(),
                    digest: location.digest,
                })))?;
        }
        let acknowledge = if location.volume.starts_with("legacy-") {
            super::Root::acknowledge_legacy
        } else {
            super::Root::acknowledge_retirement
        };
        acknowledge(
            root,
            &location.relative_key,
            &location.file_identity,
            location.bytes,
            location.digest,
            &job.operation,
        )?;
        self.inner
            .catalog
            .volume_location(Request::RecordingRetention(Action::Acknowledge(
                job.operation.clone(),
            )))?;
        Ok(())
    }
}
