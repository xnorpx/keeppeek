use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
mod speaker;

use crate::cameras::reolink::ReolinkDoorbellCapabilities;

use super::{
    CameraConfig, CameraEntry, MAX_CAMERA_METADATA_WORKERS, ServerState, probe_onvif_camera,
};

const PENDING_MAX: usize = 128;

impl CameraEntry {
    pub(super) fn speaker_available(&self) -> bool {
        if self.info.is_reolink {
            self.reolink_talk_permitted && self.reolink_profile_verified
        } else {
            self.info.capabilities.two_way_audio
        }
    }

    fn may_read_audio_outputs(
        &self,
        answered_metadata: bool,
        wake: Option<crate::battery_wake::BatteryWakeHealth>,
    ) -> bool {
        if wake.is_some_and(|state| state.registered || state.wake_pending_age_ms.is_some()) {
            return false;
        }
        // A wired Reolink may also supply a P2P UID. The existing metadata request
        // confirms it is awake before this worker makes any additional reads.
        self.battery_uid.is_none()
            || (answered_metadata
                && self.configuration.transport == crate::cameras::CameraTransport::Tcp)
    }
}

#[derive(Default)]
pub(super) struct Queue {
    state: Mutex<Pending>,
    idle: Condvar,
}

#[derive(Default)]
struct Pending {
    entries: VecDeque<CameraEntry>,
    workers: usize,
}

pub(super) fn enqueue(state: &ServerState, configs: Vec<CameraConfig>) {
    let mut pending = state
        .camera_metadata
        .state
        .lock()
        .expect("camera metadata queue is not poisoned");
    for config in configs {
        let Some(entry) = state.camera(&config.ip.to_string()) else {
            continue;
        };
        if let Some(queued) = pending
            .entries
            .iter_mut()
            .find(|queued| queued.info.id == entry.info.id)
        {
            *queued = entry;
        } else if pending.entries.len() < PENDING_MAX {
            pending.entries.push_back(entry);
        } else {
            tracing::warn!(ip = %config.ip, "camera metadata queue is full");
        }
    }
    let available = MAX_CAMERA_METADATA_WORKERS - pending.workers;
    for _ in 0..available.min(pending.entries.len()) {
        let worker = state.clone();
        pending.workers += 1;
        let spawn = std::thread::Builder::new()
            .name("camera-metadata".to_owned())
            .spawn(move || run(&worker));
        if let Err(error) = spawn {
            pending.workers -= 1;
            tracing::warn!(%error, "camera metadata worker could not start");
        }
    }
}

fn run(state: &ServerState) {
    loop {
        let entry = {
            let mut pending = state
                .camera_metadata
                .state
                .lock()
                .expect("camera metadata queue is not poisoned");
            let Some(entry) = pending.entries.pop_front() else {
                pending.workers -= 1;
                state.camera_metadata.idle.notify_all();
                return;
            };
            entry
        };
        if !current(state, &entry) {
            continue;
        }
        let answered_metadata = match probe_onvif_camera(&entry.configuration) {
            Ok(probe) => {
                state.apply_camera_metadata(&entry, &probe);
                true
            }
            Err(_) => {
                tracing::debug!(ip = %entry.configuration.ip, "configured camera did not provide ONVIF metadata");
                false
            }
        };
        let wake = state
            .battery_wake
            .as_ref()
            .zip(entry.battery_uid.as_deref())
            .map(|(wake, uid)| wake.health(uid));
        if let Some(control) = entry.control.as_ref()
            && current(state, &entry)
        {
            let capabilities = if entry.may_read_audio_outputs(answered_metadata, wake) {
                super::logged_in_reolink(control)
                    .map(|mut client| {
                        let capabilities = client.get_audio_output_capabilities(
                            u32::from(crate::reolink::CONFIGURED_CHANNEL),
                            &control.username,
                        );
                        let _ = client.logout();
                        capabilities
                    })
                    .unwrap_or_default()
            } else {
                ReolinkDoorbellCapabilities::default()
            };
            apply_audio_capabilities(state, &entry, capabilities);
            state.probe_reolink_speaker(&entry);
        }
        if current(state, &entry) {
            state.probe_hikvision_capabilities(entry.configuration.ip);
        }
    }
}

fn apply_audio_capabilities(
    state: &ServerState,
    previous: &CameraEntry,
    capabilities: ReolinkDoorbellCapabilities,
) {
    let mut cameras = state
        .cameras
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(camera) = cameras.iter_mut().find(|camera| {
        camera.info.id == previous.info.id
            && std::sync::Arc::ptr_eq(&camera.control_revision, &previous.control_revision)
    }) {
        let before = camera.speaker_available();
        let different_assets = |previous: &[crate::cameras::CameraAudioAsset],
                                next: &[(String, String)]| {
            !previous
                .iter()
                .map(|asset| (&asset.asset_id, &asset.display_name))
                .eq(next.iter().map(|(id, label)| (id, label)))
        };
        let assets_changed =
            different_assets(
                &camera.info.capabilities.quick_replies,
                &capabilities.quick_reply_assets,
            ) || different_assets(&camera.info.capabilities.chimes, &capabilities.chime_assets);
        camera.reolink_talk_permitted = capabilities.talkback;
        if !camera.reolink_talk_permitted {
            camera.reolink_profile_verified = false;
        }
        capabilities.apply_audio_assets(&mut camera.info.capabilities);
        let changed = assets_changed || before != camera.speaker_available();
        drop(cameras);
        if changed {
            state.webrtc.refresh_camera_capabilities(&previous.info.id);
        }
    }
}

fn current(state: &ServerState, entry: &CameraEntry) -> bool {
    state.camera(&entry.info.id).is_some_and(|current| {
        std::sync::Arc::ptr_eq(&current.control_revision, &entry.control_revision)
    })
}

#[cfg(test)]
impl Queue {
    pub(super) fn wait_for_idle(&self, timeout: std::time::Duration) -> bool {
        let (pending, _) = self
            .idle
            .wait_timeout_while(
                self.state
                    .lock()
                    .expect("camera metadata queue is not poisoned"),
                timeout,
                |state| state.workers > 0 || !state.entries.is_empty(),
            )
            .expect("camera metadata queue is not poisoned");
        pending.workers == 0 && pending.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cameras::CameraBackend;

    #[test]
    fn audio_inventory_and_talk_permission_follow_the_configured_camera_revision() {
        let state = super::super::tests::media_test_state();
        let mut config = state.camera("127.0.0.1").unwrap().configuration;
        config.backend = CameraBackend::ReoProto;
        state.upsert_camera(super::super::camera_entry(&config, None));
        let current = state.camera("127.0.0.1").unwrap();
        let inventory = || ReolinkDoorbellCapabilities {
            talkback: true,
            quick_reply_assets: vec![("2".into(), "Delivery".into())],
            chime_assets: vec![("hall".into(), "Hallway".into())],
            ..Default::default()
        };

        apply_audio_capabilities(&state, &current, inventory());
        let updated = state.camera("127.0.0.1").unwrap();
        assert!(updated.reolink_talk_permitted);
        assert!(!updated.info.capabilities.two_way_audio);
        assert_eq!(updated.info.capabilities.quick_replies[0].asset_id, "2");
        assert_eq!(updated.info.capabilities.chimes[0].display_name, "Hallway");

        apply_audio_capabilities(&state, &current, Default::default());
        let cleared = state.camera("127.0.0.1").unwrap();
        assert!(!cleared.reolink_talk_permitted);
        assert!(cleared.info.capabilities.quick_replies.is_empty());
        assert!(cleared.info.capabilities.chimes.is_empty());

        state.upsert_camera(super::super::camera_entry(&config, None));
        apply_audio_capabilities(&state, &current, inventory());
        let replacement = state.camera("127.0.0.1").unwrap();
        assert!(!replacement.reolink_talk_permitted);
        assert!(replacement.info.capabilities.quick_replies.is_empty());
        assert!(replacement.info.capabilities.chimes.is_empty());
    }
}
