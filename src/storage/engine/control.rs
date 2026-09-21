//! Serializes recording controls with the existing decision-and-enqueue boundary.

use super::{RecordingAdmission, StorageHandle};
use crate::storage::recording_control::{Clock, Control, Override, Revision, Snapshot};

impl RecordingAdmission {
    fn with_control(
        &self,
        camera_id: &str,
        clock: Option<Clock>,
        operation: impl FnOnce(&mut Control, Clock) -> anyhow::Result<()>,
    ) -> anyhow::Result<Snapshot> {
        let mut policies = self
            .policies
            .write()
            .map_err(|_| anyhow::anyhow!("recording admission is unavailable"))?;
        let policy = policies
            .get_mut(camera_id)
            .ok_or_else(|| anyhow::anyhow!("recording source is unavailable"))?;
        // Sample time inside the admission lock so scheduling cannot reverse observations.
        #[cfg(test)]
        let clock = clock.or_else(|| *self.control_clock.lock().unwrap());
        let now = clock.unwrap_or_else(Clock::now);
        policy.sync_permission(now);
        let result = operation(&mut policy.control, now);
        policy.sync_permission(now);
        result?;
        Ok(policy.control.snapshot(now))
    }

    #[cfg(test)]
    pub(super) fn control_snapshot(&self, camera_id: &str, now: Clock) -> anyhow::Result<Snapshot> {
        self.with_control(camera_id, Some(now), |_, _| Ok(()))
    }

    #[cfg(test)]
    pub(super) fn set_override(
        &self,
        camera_id: &str,
        revision: Revision,
        request: Override,
        now: Clock,
    ) -> anyhow::Result<Snapshot> {
        self.with_control(camera_id, Some(now), |control, now| {
            control.set_override(revision, request, now)
        })
    }

    #[cfg(test)]
    pub(super) fn set_privacy(
        &self,
        camera_id: &str,
        active: Option<bool>,
        now: Clock,
    ) -> anyhow::Result<Snapshot> {
        self.with_control(camera_id, Some(now), |control, _| {
            control.set_privacy(active);
            Ok(())
        })
    }
}

impl StorageHandle {
    #[cfg(test)]
    pub(crate) fn set_control_clock_for_test(&self, now: Clock) {
        *self.admission.control_clock.lock().unwrap() = Some(now);
    }

    pub fn recording_control(&self, camera_id: &str) -> anyhow::Result<Snapshot> {
        self.admission.with_control(camera_id, None, |_, _| Ok(()))
    }

    /// The server must authorize the actor before this atomic mutation.
    pub fn set_recording_override(
        &self,
        camera_id: &str,
        revision: Revision,
        request: Override,
    ) -> anyhow::Result<Snapshot> {
        self.admission
            .with_control(camera_id, None, |control, now| {
                control.set_override(revision, request, now)
            })
    }

    pub fn clear_recording_override(
        &self,
        camera_id: &str,
        revision: Revision,
    ) -> anyhow::Result<Snapshot> {
        self.admission
            .with_control(camera_id, None, |control, now| {
                control.clear_override(revision, now)
            })
    }

    /// A required privacy authority must publish `None` while its state is unavailable.
    pub fn set_recording_privacy(
        &self,
        camera_id: &str,
        active: Option<bool>,
    ) -> anyhow::Result<Snapshot> {
        self.admission.with_control(camera_id, None, |control, _| {
            control.set_privacy(active);
            Ok(())
        })
    }
}
