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
    stalled: bool,
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
        let stalled = queue.stalled;
        drop(queue);
        if stalled {
            report_failure(
                state,
                registry,
                "Camera runtime has not acknowledged the preceding transition",
            );
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
        let (desired, response) = {
            // Serialize submission with camera edits. Never wait on the recorder under this lock.
            let _guard = state
                .config_update
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut queue = state
                .profile_application
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            queue.stalled = false;
            let Some(desired) = queue.next.take() else {
                queue.running = false;
                return;
            };
            drop(queue);
            let identities = match current_identities(state, &desired) {
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
            (desired, response)
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
                        queue.stalled = true;
                        let registry = queue
                            .next
                            .as_ref()
                            .map_or(&desired.registry, |next| &next.registry)
                            .clone();
                        drop(queue);
                        report_failure(
                            state,
                            &registry,
                            "Camera runtime did not acknowledge the requested transition in time",
                        );
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
        match current_identities(state, &desired) {
            Ok(Some(ids)) => {
                let result = result.and_then(|applications| {
                    let privacy = confirmed_privacy(state, &desired.registry, &ids, &applications)?;
                    state.privacy.replace_profile_overrides(privacy)?;
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
            Ok(None) => {}
            Err(error) => report_failure(state, &desired.registry, &error.to_string()),
        }
    }
}

fn current_identities(
    state: &ServerState,
    desired: &Desired,
) -> anyhow::Result<Option<crate::keeppeek::CameraProfileIds>> {
    if state
        .runtime_profiles
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .revision
        != desired.registry.revision
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
        config.runtime_profiles == desired.registry && ids == desired.camera_ids,
        "Camera configuration changed during application. Reapply the current profile to retry."
    );
    Ok(Some(crate::keeppeek::CameraProfileIds::new(ids)?))
}

pub(in crate::server) fn confirmed_privacy(
    state: &ServerState,
    registry: &ProfileRegistry,
    ids: &crate::keeppeek::CameraProfileIds,
    applications: &[CameraRuntimeApplication],
) -> anyhow::Result<BTreeMap<String, bool>> {
    let held = state.privacy.profile_overrides_snapshot();
    let mut next = ids.privacy_overrides(registry);
    let current = state
        .runtime_profile_status
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    anyhow::ensure!(
        current.revision == registry.revision,
        "runtime profile transition was superseded"
    );
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
    Ok(next)
}
