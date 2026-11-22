//! Validates and saves recorded timing without restarting camera ingestion.

use super::*;
use crate::cameras::RecordedAudioSettings;

pub(super) const fn to_proto(settings: RecordedAudioSettings) -> proto::RecordedAudioSettings {
    proto::RecordedAudioSettings {
        default_offset_ms: settings.default_offset_ms,
        main_offset_ms: settings.main_offset_ms,
        sub_offset_ms: settings.sub_offset_ms,
    }
}

pub(super) fn from_proto(
    settings: proto::RecordedAudioSettings,
) -> Result<RecordedAudioSettings, ControlCommandError> {
    validated(RecordedAudioSettings {
        default_offset_ms: settings.default_offset_ms,
        main_offset_ms: settings.main_offset_ms,
        sub_offset_ms: settings.sub_offset_ms,
    })
}

pub(super) fn merged(
    update: Option<RecordedAudioSettings>,
    existing: Option<&CameraConfig>,
) -> Result<RecordedAudioSettings, ControlCommandError> {
    validated(
        update
            .or_else(|| existing.map(|camera| camera.recorded_audio))
            .unwrap_or_default(),
    )
}

fn validated(
    settings: RecordedAudioSettings,
) -> Result<RecordedAudioSettings, ControlCommandError> {
    settings.validate().map_err(|error| {
        ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
    })?;
    Ok(settings)
}

pub(super) const fn only(update: &CameraSettingsUpdate) -> bool {
    matches!(
        update,
        CameraSettingsUpdate {
            recorded_audio: Some(_),
            expected_configuration_revision: _,
            display_name: None,
            manufacturer: None,
            username: None,
            password: None,
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: None,
            transport: None,
            record_generic_motion_events: None,
            recording_mode: None,
            event_recording_duration_secs: None,
            event_pre_recording_duration_secs: None,
            event_recording_stream: None,
        }
    )
}

pub(super) fn save(
    update: &CameraSettingsUpdate,
    state: &ServerState,
    camera_id: &str,
) -> Result<CameraSettingsUpdateResponse, ControlCommandError> {
    let settings = merged(update.recorded_audio, None)?;
    let ip = camera_id.parse::<IpAddr>().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "camera ID must be an IP address",
        )
    })?;
    let path = state.camera_config_path.as_deref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "camera configuration persistence is unavailable",
        )
    })?;
    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    check_revision(state, &update.expected_configuration_revision)?;
    let mut camera = load_camera(path, ip)?;
    camera.recorded_audio = settings;
    config::upsert_camera(path, &camera).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to save recorded audio settings",
        )
    })?;
    apply(state, camera_id, settings);
    let entry = state.camera(camera_id);
    Ok(CameraSettingsUpdateResponse {
        camera: camera_settings_entry(&camera, entry.as_ref(), None, Some(path)),
        restart_required: false,
        configuration_revision: camera_configuration_revision(state)?,
    })
}

fn check_revision(state: &ServerState, expected: &str) -> Result<(), ControlCommandError> {
    let revision = camera_configuration_revision(state)?;
    if !expected.is_empty() && expected != revision {
        return Err(configuration::revision_conflict(
            &revision,
            "camera configuration changed; reload current values before retrying",
        ));
    }
    Ok(())
}

fn load_camera(path: &Path, ip: IpAddr) -> Result<CameraConfig, ControlCommandError> {
    config::load_cameras(path)
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                "unable to load camera configuration",
            )
        })?
        .into_values()
        .flatten()
        .find(|camera| camera.ip == ip)
        .ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "camera not found")
        })
}

fn apply(state: &ServerState, camera_id: &str, settings: RecordedAudioSettings) {
    let mut cameras = state
        .cameras
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(camera) = cameras
        .iter_mut()
        .find(|camera| camera.info.id == camera_id)
    {
        camera.configuration.recorded_audio = settings;
        if let Some(runtime) = &mut camera.runtime_camera {
            runtime.config.recorded_audio = settings;
        }
    }
}
