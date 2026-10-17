use super::*;
use crate::{profiles::RuntimeApplicationState, storage::RecordingConfiguration};
use std::time::Instant;

const RECORDING_ACK_TIMEOUT: Duration = Duration::from_millis(200);

pub(super) type RecordingRequest = Result<Option<RecordingConfiguration>, String>;

pub(super) struct PendingCameraRecording<'a> {
    pub(super) camera: &'a Camera,
    pub(super) configuration: RecordingConfiguration,
    pub(super) runtime_result: Result<(), String>,
}

pub(super) fn recording_request_state(
    request: &RecordingRequest,
) -> Result<RuntimeApplicationState, String> {
    request
        .as_ref()
        .map(|configuration| {
            if configuration.is_some() {
                RuntimeApplicationState::Pending
            } else {
                RuntimeApplicationState::Applied
            }
        })
        .map_err(Clone::clone)
}

impl KeepPeekLoop {
    pub(super) fn configure_profile_recording(
        &self,
        camera: &Camera,
    ) -> anyhow::Result<RecordingRequest> {
        let status = self
            .runtime_profile_status
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(
            status.revision <= self.runtime_profiles.revision,
            "runtime profile transition was superseded"
        );
        Ok(self.storage.as_ref().map_or(Ok(None), |storage| {
            storage
                .request_camera_event_recording(
                    &camera.config.ip.to_string(),
                    self.effective_recording_mode(&camera.config),
                    camera.config.event_recording_stream,
                    Duration::from_secs(camera.config.event_pre_recording_duration_secs),
                    Duration::from_secs(camera.config.event_recording_duration_secs),
                )
                .map(Some)
                .map_err(|error| error.to_string())
        }))
    }

    pub(super) fn camera_runtime_application(
        &self,
        camera: &Camera,
        policy: crate::profiles::EffectiveCameraPolicy,
        result: Result<RuntimeApplicationState, String>,
    ) -> crate::profiles::CameraRuntimeApplication {
        let result = result.and_then(|state| {
            if policy.notifications_enabled {
                Ok(state)
            } else {
                self.runtime_profile_notification_error
                    .clone()
                    .map_or(Ok(state), Err)
            }
        });
        crate::profiles::CameraRuntimeApplication {
            camera_id: camera.config.ip.to_string(),
            state: result
                .as_ref()
                .cloned()
                .unwrap_or(RuntimeApplicationState::Degraded),
            policy,
            reason: result
                .err()
                .map(|reason| reason.chars().take(256).collect()),
        }
    }

    pub(super) fn record_camera_runtime_state(
        &self,
        camera: &Camera,
        result: Result<RuntimeApplicationState, String>,
    ) {
        let application = self.camera_runtime_application(
            camera,
            self.effective_camera_policy(&camera.config),
            result,
        );
        let mut status = self
            .runtime_profile_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status.revision > self.runtime_profiles.revision {
            return;
        }
        status.revision = self.runtime_profiles.revision;
        status.active_profile_id = self.runtime_profiles.active_profile_id.clone();
        status
            .cameras
            .insert(application.camera_id.clone(), application);
        drop(status);
        self.publish_runtime_profile_status();
    }

    pub(super) fn finish_camera_recordings(&self, pending: Vec<PendingCameraRecording<'_>>) {
        // ponytail: one deadline bounds the whole startup batch, including a stalled writer.
        let deadline = Instant::now() + RECORDING_ACK_TIMEOUT;
        for pending in pending {
            let recording_result = pending
                .configuration
                .wait_until(deadline)
                .map_err(|error| error.to_string());
            self.record_camera_runtime_status(
                pending.camera,
                pending.runtime_result.and(recording_result),
            );
        }
    }

    pub(super) fn finish_profile_recordings(
        &self,
        applications: &mut [crate::profiles::CameraRuntimeApplication],
        pending: Vec<(usize, RecordingConfiguration)>,
    ) {
        let deadline = Instant::now() + RECORDING_ACK_TIMEOUT;
        for (index, configuration) in pending {
            let result = configuration.wait_until(deadline);
            let application = &mut applications[index];
            if application.state != RuntimeApplicationState::Pending {
                continue;
            }
            match result {
                Ok(()) => application.state = RuntimeApplicationState::Applied,
                Err(error) => {
                    application.state = RuntimeApplicationState::Degraded;
                    application.reason = Some(error.to_string().chars().take(256).collect());
                }
            }
        }
    }
}
