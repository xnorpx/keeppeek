use super::{CameraEntry, ServerState};
use std::{net::SocketAddr, sync::Arc};

impl ServerState {
    /// Runs only on the bounded metadata queue, after CGI permission was observed for this entry.
    pub(in crate::server) fn probe_reolink_speaker(&self, previous: &CameraEntry) {
        let Some(camera) = self.camera(&previous.info.id) else { return };
        if !Arc::ptr_eq(&camera.control_revision, &previous.control_revision) {
            return;
        }
        let config = &camera.configuration;
        let Some(channel) = crate::keeppeek::powered_reolink_channel(config, camera.info.is_reolink) else {
            return;
        };
        if !camera.reolink_talk_permitted {
            self.apply_reolink_speaker(&camera, false);
            return;
        }
        let address = SocketAddr::new(config.ip, crate::cameras::BAICHUAN_PORT);
        let compatible = crate::reolink::speaker_probe::compatible(address, &config.username, &config.password, channel).unwrap_or(false);
        self.apply_reolink_speaker(&camera, compatible);
    }

    fn apply_reolink_speaker(&self, previous: &CameraEntry, compatible: bool) {
        let mut entries = self.cameras.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(camera) = entries.iter_mut().find(|entry| entry.info.id == previous.info.id && Arc::ptr_eq(&entry.control_revision, &previous.control_revision)) else { return };
        let before = camera.speaker_available();
        camera.reolink_profile_verified = compatible && camera.reolink_talk_permitted;
        let changed = before != camera.speaker_available();
        drop(entries);
        if changed {
            self.webrtc.refresh_camera_capabilities(&previous.info.id);
        }
    }
}
