use super::*;
use crate::profiles::{CameraRuntimeApplication, ProfileRegistry, RuntimeApplicationState};
use std::{net::IpAddr, sync::mpsc::RecvTimeoutError};

#[derive(Clone)]
struct Desired {
    expected_config: Option<[u8; 32]>,
    registry: ProfileRegistry,
    camera_ids: BTreeMap<IpAddr, String>,
}

#[derive(Default)]
pub(in crate::server) struct ProfileApplicationQueue {
    next: Option<Desired>,
    recoverable: Option<Desired>,
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
    queue.recoverable = None;
    queue.next = Some(Desired {
        registry: registry.clone(),
        camera_ids,
        expected_config: None,
    });
    start_queued_worker(state, queue);
}

impl ServerState {
    // Called after the recovered recorder and its recording configuration have both acknowledged.
    pub(crate) fn confirm_recovered_profile(&self, revision: u64) {
        let mut queue = self
            .profile_application
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if queue.next.is_some() {
            return;
        }
        let Some(desired) = queue
            .recoverable
            .take_if(|request| request.registry.revision == revision)
        else {
            return;
        };
        queue.next = Some(desired);
        start_queued_worker(self, queue);
    }
}

fn start_queued_worker(
    state: &ServerState,
    mut queue: std::sync::MutexGuard<'_, ProfileApplicationQueue>,
) {
    if queue.running {
        if queue.stalled_revision.is_some() {
            queue.stalled_revision = queue.next.as_ref().map(|desired| desired.registry.revision);
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
        let failed = queue
            .next
            .take()
            .expect("profile worker requires a queued request");
        drop(queue);
        report_failure(state, &failed.registry, &error.to_string());
    }
}

fn report_failure(state: &ServerState, registry: &ProfileRegistry, reason: &str) {
    update_runtime_status(state, registry, None, Some(reason.to_owned()), false);
    publish_status_event(state, now_ms());
}

fn report_superseded_recovery(state: &ServerState, registry: &ProfileRegistry) {
    let mut current = state
        .runtime_profile_status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if current.revision != registry.revision {
        return;
    }
    for camera in current.cameras.values_mut() {
        if camera.state == RuntimeApplicationState::Degraded
            && camera.reason.as_deref() == Some(crate::profiles::PENDING_PRIVACY_CONFIRMATION)
        {
            camera.reason = Some("Camera configuration changed before recovery confirmation. Reapply the current profile to retry.".into());
        }
    }
    drop(current);
    publish_status_event(state, now_ms());
}

type ProfileReply =
    anyhow::Result<std::sync::mpsc::Receiver<crate::keeppeek::ProfileApplicationResult>>;

fn run(state: &ServerState) {
    while let Some((desired, submitted_config, response)) = submit_next(state) {
        let result = await_application(state, desired.registry.revision, response);
        finish_application(state, &desired, submitted_config, result);
    }
}

fn submit_next(state: &ServerState) -> Option<(Desired, [u8; 32], ProfileReply)> {
    loop {
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
            return None;
        };
        drop(queue);
        let (identities, submitted_config) = match current_identities(
            state,
            &desired.registry,
            &desired.camera_ids,
            desired.expected_config,
        ) {
            Ok(Some(ids)) => ids,
            Ok(None) if desired.expected_config.is_some() => {
                report_superseded_recovery(state, &desired.registry);
                continue;
            }
            Ok(None) => continue,
            Err(error) => {
                report_failure(state, &desired.registry, &error.to_string());
                continue;
            }
        };
        // A deferred worker may finish before this command's reply reaches the application thread.
        // Install its exact registry, identities, and configuration fence before submission.
        state
            .profile_application
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recoverable = Some(Desired {
            expected_config: Some(submitted_config),
            ..desired.clone()
        });
        let runtime = state
            .camera_runtime
            .as_ref()
            .expect("application worker requires camera runtime");
        let response = runtime.submit_runtime_profiles(desired.registry.clone(), Some(identities));
        return Some((desired, submitted_config, response));
    }
}

fn await_application(
    state: &ServerState,
    revision: u64,
    response: ProfileReply,
) -> crate::keeppeek::ProfileApplicationResult {
    response.and_then(|response| {
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
                            .map_or(revision, |next| next.registry.revision),
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
    })
}

fn finish_application(
    state: &ServerState,
    desired: &Desired,
    submitted_config: [u8; 32],
    result: crate::keeppeek::ProfileApplicationResult,
) {
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
        Ok(None) => reconcile_superseded(state, desired),
        Err(error) => report_failure(state, &desired.registry, &error.to_string()),
    }
}

// This is called under config_update. A settings edit may update the camera file without
// publishing a recorder observation. Obtain a fresh acknowledgement before releasing privacy.
fn reconcile_superseded(state: &ServerState, desired: &Desired) {
    if desired.expected_config.is_some() {
        report_superseded_recovery(state, &desired.registry);
        return;
    }
    let Some(path) = &state.camera_config_path else {
        return;
    };
    let config = match config::load_config(path) {
        Ok(config) => config,
        Err(error) => return report_failure(state, &desired.registry, &error.to_string()),
    };
    if config.runtime_profiles != desired.registry {
        return;
    }
    match config.camera_profile_identities() {
        Ok(ids) => schedule(state, &desired.registry, ids),
        Err(error) => report_failure(state, &desired.registry, &error.to_string()),
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

pub(in crate::server) fn camera_fingerprint(config: &Config) -> anyhow::Result<[u8; 32]> {
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
    fn audio_edit_between_submission_and_base_acknowledgement_resubmits_before_releasing_privacy() {
        super::super::lifecycle_tests::with_applied_base(|state, path, source| {
            let original = config::load_config(path).unwrap();
            let fingerprint = camera_fingerprint(&original).unwrap();
            let desired = Desired {
                registry: original.runtime_profiles.clone(),
                camera_ids: original.camera_profile_identities().unwrap(),
                expected_config: None,
            };
            let mut camera = original
                .camera_configurations()
                .unwrap()
                .into_values()
                .flatten()
                .next()
                .unwrap();
            camera.recorded_audio.default_offset_ms = Some(50);
            config::upsert_camera(path, &camera).unwrap();
            state
                .privacy
                .replace_profile_overrides(BTreeMap::from([(source.to_owned(), true)]))
                .unwrap();
            finish_application(state, &desired, fingerprint, Ok(Vec::new()));
            let deadline = Instant::now() + Duration::from_secs(6);
            while state
                .privacy
                .decision(source, chrono::Utc::now())
                .unwrap()
                .0
            {
                assert!(
                    Instant::now() < deadline,
                    "fresh camera acknowledgement did not release privacy"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        });
    }

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
