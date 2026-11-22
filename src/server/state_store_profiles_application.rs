use super::*;
use crate::profiles::{CameraRuntimeApplication, ProfileRegistry, RuntimeApplicationState};
use std::{net::IpAddr, sync::mpsc::RecvTimeoutError};

struct Desired {
    registry: ProfileRegistry,
    camera_ids: BTreeMap<IpAddr, String>,
}

#[derive(Default)]
pub(in crate::server) struct ProfileApplicationQueue {
    next: Option<Desired>,
    running: bool,
    stalled_revision: Option<u64>,
}

// Called after durable persistence and pending publication, under the config update lock.
pub(super) fn schedule(
    state: &ServerState,
    registry: &ProfileRegistry,
    camera_ids: BTreeMap<IpAddr, String>,
) {
    if state.camera_runtime.is_none() {
        update_runtime_status(state, registry, None, None, true);
        publish_status_event(state, now_ms());
        return;
    }
    let mut queue = state
        .profile_application
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    queue.next = Some(Desired {
        registry: registry.clone(),
        camera_ids,
    });
    if queue.running {
        if queue.stalled_revision.is_some() {
            queue.stalled_revision = Some(registry.revision);
            drop(queue);
            publish_status_event(state, now_ms());
        }
        return;
    }
    queue.running = true;
    let worker = state.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("profile-application".to_owned())
        .spawn(move || run(&worker))
    {
        queue.running = false;
        queue.next = None;
        drop(queue);
        report_failure(state, registry, &error.to_string());
    }
}

fn report_failure(state: &ServerState, registry: &ProfileRegistry, reason: &str) {
    update_runtime_status(state, registry, None, Some(reason.to_owned()), false);
    publish_status_event(state, now_ms());
}

fn run(state: &ServerState) {
    loop {
        let (desired, submitted_config, response) = {
            // Serialize submission with camera edits. Never wait on the recorder under this lock.
            let _guard = state
                .config_update
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut queue = state
                .profile_application
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            queue.stalled_revision = None;
            let Some(desired) = queue.next.take() else {
                queue.running = false;
                return;
            };
            drop(queue);
            let (identities, submitted_config) =
                match current_identities(state, &desired.registry, &desired.camera_ids, None) {
                    Ok(Some(ids)) => ids,
                    Ok(None) => continue,
                    Err(error) => {
                        report_failure(state, &desired.registry, &error.to_string());
                        continue;
                    }
                };
            let runtime = state
                .camera_runtime
                .as_ref()
                .expect("application worker requires camera runtime");
            let response =
                runtime.submit_runtime_profiles(desired.registry.clone(), Some(identities));
            (desired, submitted_config, response)
        };
        let result = response.and_then(|response| {
            match response.recv_timeout(crate::keeppeek::CAMERA_CONTROL_TIMEOUT) {
                Ok(result) => result,
                Err(RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("Camera runtime stopped before replying")
                }
                Err(RecvTimeoutError::Timeout) => {
                    {
                        let _guard = state
                            .config_update
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let mut queue = state
                            .profile_application
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        queue.stalled_revision = Some(
                            queue
                                .next
                                .as_ref()
                                .map_or(desired.registry.revision, |next| next.registry.revision),
                        );
                        drop(queue);
                        publish_status_event(state, now_ms());
                    }
                    // Keep only one command in flight. A late reply can recover; a new desired
                    // selection replaces the queued selection even while the recorder is stalled.
                    response
                        .recv()
                        .map_err(|_| anyhow::anyhow!("Camera runtime stopped before replying"))?
                }
            }
        });
        let _guard = state
            .config_update
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .profile_application
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stalled_revision = None;
        match current_identities(
            state,
            &desired.registry,
            &desired.camera_ids,
            Some(submitted_config),
        ) {
            Ok(Some((ids, _))) => {
                let result = result.and_then(|applications| {
                    apply_confirmed_privacy(state, &desired.registry, &ids, &applications, None)?;
                    Ok(applications)
                });
                match result {
                    Ok(applications) => {
                        update_runtime_status(
                            state,
                            &desired.registry,
                            Some(applications),
                            None,
                            false,
                        );
                        publish_status_event(state, now_ms());
                    }
                    Err(error) => report_failure(state, &desired.registry, &error.to_string()),
                }
            }
            Ok(None) => publish_status_event(state, now_ms()),
            Err(error) => report_failure(state, &desired.registry, &error.to_string()),
        }
    }
}

pub(super) fn current_identities(
    state: &ServerState,
    registry: &ProfileRegistry,
    expected_ids: &BTreeMap<IpAddr, String>,
    submitted_fingerprint: Option<[u8; 32]>,
) -> anyhow::Result<Option<(crate::keeppeek::CameraProfileIds, [u8; 32])>> {
    if state
        .runtime_profiles
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .revision
        != registry.revision
    {
        return Ok(None);
    }
    let path = state
        .camera_config_path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Camera configuration is unavailable"))?;
    let config = config::load_config(path)?;
    let ids = config.camera_profile_identities()?;
    anyhow::ensure!(
        config.runtime_profiles == *registry,
        "Camera configuration changed during application. Reapply the current profile to retry."
    );
    let fingerprint = camera_fingerprint(&config)?;
    if submitted_fingerprint.is_some_and(|before| before != fingerprint || &ids != expected_ids) {
        // The camera edit publishes its own observations under the same selection revision.
        return Ok(None);
    }
    anyhow::ensure!(
        &ids == expected_ids,
        "Camera configuration changed during application. Reapply the current profile to retry."
    );
    Ok(Some((
        crate::keeppeek::CameraProfileIds::new(ids)?,
        fingerprint,
    )))
}

pub(super) fn camera_fingerprint(config: &Config) -> anyhow::Result<[u8; 32]> {
    use sha2::Digest as _;
    let cameras: BTreeMap<_, _> = config.camera_configurations()?.into_iter().collect();
    Ok(sha2::Sha256::digest(serde_json::to_vec(&cameras)?).into())
}

/// Report a delayed command reply separately from the camera loop's own observations.
pub(super) fn observed_status<'a>(
    state: &ServerState,
    status: &'a crate::profiles::RuntimeProfileStatus,
) -> std::borrow::Cow<'a, crate::profiles::RuntimeProfileStatus> {
    if state
        .profile_application
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .stalled_revision
        != Some(status.revision)
    {
        return std::borrow::Cow::Borrowed(status);
    }
    let mut observation = status.clone();
    for application in observation.cameras.values_mut() {
        if application.state != RuntimeApplicationState::Degraded {
            application.state = RuntimeApplicationState::Degraded;
            application.reason = Some("Camera runtime acknowledgment timed out".into());
        }
    }
    std::borrow::Cow::Owned(observation)
}

pub(in crate::server) fn apply_confirmed_privacy(
    state: &ServerState,
    registry: &ProfileRegistry,
    ids: &crate::keeppeek::CameraProfileIds,
    applications: &[CameraRuntimeApplication],
    stopped: Option<IpAddr>,
) -> anyhow::Result<()> {
    let current = state
        .runtime_profile_status
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    anyhow::ensure!(
        current.revision == registry.revision,
        "runtime profile transition was superseded"
    );
    let held = state.privacy.profile_overrides_snapshot();
    let mut next = ids.privacy_overrides(registry);
    let applied: BTreeSet<_> = applications
        .iter()
        .filter(|application| {
            application.state == RuntimeApplicationState::Applied
                && current
                    .cameras
                    .get(&application.camera_id)
                    .is_some_and(|observed| observed.state == RuntimeApplicationState::Applied)
        })
        .filter_map(|application| ids.source_id(&application.camera_id))
        .collect();
    for (source, enforced) in held {
        if enforced && !applied.contains(source.as_str()) {
            next.insert(source, true);
        }
    }
    if let Some(stopped) = stopped {
        next.remove(&stopped.to_string());
    }
    state.privacy.replace_profile_overrides(next)?;
    drop(current);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_acknowledgment_projects_a_timeout_without_replacing_the_recorder_observation() {
        let state = ServerState::empty();
        let policy = crate::profiles::resolve_camera_policy(
            &crate::profiles::BaseCameraPolicy {
                available: true,
                recording_mode: crate::cameras::CameraRecordingMode::Main,
                privacy_enforced: false,
                notifications_enabled: true,
            },
            None,
        );
        let camera = CameraRuntimeApplication {
            camera_id: "camera".into(),
            state: RuntimeApplicationState::Applied,
            policy,
            reason: None,
        };
        let mut actual = crate::profiles::RuntimeProfileStatus {
            revision: 4,
            cameras: BTreeMap::from([(camera.camera_id.clone(), camera.clone())]),
            ..Default::default()
        };
        state.profile_application.lock().unwrap().stalled_revision = Some(4);
        let shown = observed_status(&state, &actual);
        assert_eq!(
            shown.cameras["camera"].state,
            RuntimeApplicationState::Degraded
        );
        assert_eq!(actual.cameras["camera"], camera);
        drop(shown);
        let failed = actual.cameras.get_mut("camera").unwrap();
        failed.state = RuntimeApplicationState::Degraded;
        failed.reason = Some("recording stopped".into());
        assert_eq!(
            observed_status(&state, &actual).cameras["camera"]
                .reason
                .as_deref(),
            Some("recording stopped")
        );
        actual
            .cameras
            .insert(camera.camera_id.clone(), camera.clone());
        state.profile_application.lock().unwrap().stalled_revision = None;
        assert_eq!(observed_status(&state, &actual).cameras["camera"], camera);
    }
}
