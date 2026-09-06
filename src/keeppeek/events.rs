use crate::cameras::CameraConfig;

pub(super) fn event_only_update(previous: &CameraConfig, next: &CameraConfig) -> bool {
    if previous.events == next.events
        && previous.record_generic_motion_events == next.record_generic_motion_events
    {
        return false;
    }
    let (Ok(mut previous), Ok(mut next)) =
        (serde_json::to_value(previous), serde_json::to_value(next))
    else {
        return false;
    };
    for value in [&mut previous, &mut next] {
        let Some(fields) = value.as_object_mut() else {
            return false;
        };
        fields.remove("events");
        fields.remove("record_generic_motion_events");
    }
    previous == next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cameras::{
        CameraBackend, CameraTransport,
        events::{EventConfig, EventMode},
    };

    fn config() -> CameraConfig {
        CameraConfig {
            events: EventConfig::default(),
            ip: "192.0.2.1".parse().unwrap(),
            name: None,
            display_name: None,
            manufacturer: None,
            username: "test".to_owned(),
            password: "test".to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Retina,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: Default::default(),
            event_recording_duration_secs: 60,
        }
    }

    #[test]
    fn unchanged_settings_still_require_an_explicit_media_restart() {
        let previous = config();
        assert!(!event_only_update(&previous, &previous));
        let mut next = previous.clone();
        next.events.mode = EventMode::Disabled;
        assert!(event_only_update(&previous, &next));
        next.main_rtsp_url = Some("rtsp://192.0.2.1/main".to_owned());
        assert!(!event_only_update(&previous, &next));
        let mut retention = previous.clone();
        retention.record_generic_motion_events = true;
        assert!(event_only_update(&previous, &retention));
    }
}
