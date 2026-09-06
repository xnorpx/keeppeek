use super::{CameraEntry, ControlCommandError, MotionDetection, ReolinkClient, ServerState, proto};

mod capabilities;
mod ptz;
pub(super) use capabilities::{Report, ptz_capability};
pub(super) use ptz::{Owner, close_session, handle_ptz};

pub(super) fn available(camera: &CameraEntry) -> bool {
    camera.control.is_some() || hikvision_route(camera).is_some()
}

pub(super) enum Movement {
    Hikvision(::isapi::Ptz),
    Reolink(super::PtzOp, u32),
}

impl Movement {
    pub fn send(self, camera: &CameraEntry) -> anyhow::Result<()> {
        match self {
            Self::Hikvision(velocity) => {
                let route = hikvision_route(camera)
                    .ok_or_else(|| anyhow::anyhow!("Hikvision control is unavailable"))?;
                let response = hikvision_client(camera, &route)?
                    .command(&velocity.continuous(route.channel)?)?;
                anyhow::ensure!(
                    !response.reboot_required(),
                    "camera did not confirm immediate PTZ operation"
                );
                Ok(())
            }
            Self::Reolink(operation, speed) => super::reolink_ptz(
                camera
                    .control
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Reolink control is unavailable"))?,
                operation,
                speed,
            ),
        }
    }
}

pub(super) fn movement(
    camera: &CameraEntry,
    movement: &proto::PtzContinuous,
) -> Result<Movement, ControlCommandError> {
    if hikvision_route(camera).is_some() {
        let axes = [movement.pan, movement.tilt, movement.zoom];
        if axes
            .iter()
            .any(|axis| !axis.is_finite() || !(-1.0..=1.0).contains(axis))
            || axes.iter().all(|axis| *axis == 0.0)
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "PTZ axes must be finite, normalized, and not all zero",
            ));
        }
        let capabilities = camera
            .hikvision
            .as_ref()
            .and_then(|report| report.ptz.as_ref())
            .ok_or_else(|| unsupported("camera PTZ capabilities are not verified"))?;
        let pan = axis_speed(movement.pan, capabilities.pan_range())?;
        let tilt = axis_speed(movement.tilt, capabilities.tilt_range())?;
        let zoom = axis_speed(movement.zoom, capabilities.zoom_range())?;
        let velocity = ::isapi::Ptz::new(pan, tilt, zoom).map_err(|_| {
            ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, "invalid PTZ speed")
        })?;
        Ok(Movement::Hikvision(velocity))
    } else {
        let (operation, speed) = super::ptz_continuous_operation(movement)?;
        Ok(Movement::Reolink(operation, speed))
    }
}

fn unsupported(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::UnsupportedRequest, 501, message)
}

fn axis_speed(axis: f32, range: Option<[i8; 2]>) -> Result<i8, ControlCommandError> {
    if axis == 0.0 {
        return Ok(0);
    }
    let [min, max] =
        range.ok_or_else(|| unsupported("camera does not report support for this PTZ axis"))?;
    let limit = if axis < 0.0 {
        f32::from(min).abs()
    } else {
        f32::from(max)
    };
    if limit == 0.0 {
        return Err(unsupported(
            "camera does not report support for this PTZ direction",
        ));
    }
    Ok((axis.signum() * (axis.abs() * limit).round().max(1.0)) as i8)
}

pub(super) fn stop(camera: &CameraEntry) -> anyhow::Result<()> {
    if hikvision_route(camera).is_some() {
        Movement::Hikvision(::isapi::Ptz::new(0, 0, 0)?).send(camera)
    } else {
        Movement::Reolink(super::PtzOp::Stop, super::PTZ_STOP_SPEED).send(camera)
    }
}

pub(super) fn presets(camera: &CameraEntry) -> Result<Vec<proto::PtzPreset>, ControlCommandError> {
    if !ptz_capability(camera).presets {
        return Err(unsupported("camera does not report preset support"));
    }
    if let Some(route) = hikvision_route(camera) {
        let result = (|| -> anyhow::Result<_> {
            let mut client = hikvision_client(camera, &route)?;
            Ok(client
                .query(&::isapi::management::Preset::list(route.channel)?)?
                .into_iter()
                .map(|preset| proto::PtzPreset {
                    preset_id: preset.id(),
                    name: preset.name().to_owned(),
                })
                .collect())
        })();
        result.map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                502,
                "camera PTZ preset query failed",
            )
        })
    } else {
        let control = camera.control.as_ref().ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                409,
                "camera PTZ transport is unavailable",
            )
        })?;
        super::reolink_ptz_presets(control)
            .map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::Unavailable,
                    502,
                    "camera PTZ preset query failed",
                )
            })?
            .into_iter()
            .map(super::proto_ptz_preset)
            .collect()
    }
}

pub(super) fn goto_preset(camera: &CameraEntry, preset: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        ptz_capability(camera).presets,
        "camera does not report preset support"
    );
    if let Some(route) = hikvision_route(camera) {
        let response = hikvision_client(camera, &route)?
            .command(&::isapi::Ptz::goto_preset(route.channel, preset)?)?;
        anyhow::ensure!(
            !response.reboot_required(),
            "camera did not confirm immediate preset operation"
        );
        Ok(())
    } else {
        super::reolink_goto_preset(
            camera
                .control
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Reolink control is unavailable"))?,
            preset,
        )
    }
}

fn hikvision_route(camera: &CameraEntry) -> Option<crate::isapi::Route> {
    if camera.info.is_reolink {
        return None;
    }
    crate::isapi::Route::for_control(
        &camera.configuration,
        camera.info.ports.http,
        camera.info.manufacturer.as_deref(),
    )
}

fn hikvision_client(
    camera: &CameraEntry,
    route: &crate::isapi::Route,
) -> anyhow::Result<::isapi::blocking::Client> {
    Ok(::isapi::blocking::Client::new(
        &route.origin,
        ::isapi::Credentials::new(
            camera.configuration.username.clone(),
            camera.configuration.password.clone(),
        ),
    )?)
}

fn motion_enabled(camera: &CameraEntry) -> anyhow::Result<bool> {
    if let Some(route) = hikvision_route(camera) {
        return Ok(hikvision_client(camera, &route)?
            .query(&::isapi::management::Motion::query(route.channel)?)?
            .enabled());
    }
    let control = camera
        .control
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("motion detection control is unavailable"))?;
    let mut client = ReolinkClient::new_with_http_port(control.ip, control.http_port);
    client.login(&control.username, &control.password)?;
    client.motion_enabled(0)
}

pub(super) fn status(camera: &CameraEntry) -> MotionDetection {
    if camera.control.is_none() && hikvision_route(camera).is_none() {
        return MotionDetection {
            supported: camera.info.capabilities.events,
            controllable: false,
            enabled: None,
            error: None,
        };
    }
    motion_enabled(camera).map_or_else(
        |_| MotionDetection {
            supported: camera.info.capabilities.events,
            controllable: false,
            enabled: None,
            error: Some("camera motion configuration could not be read".to_owned()),
        },
        |enabled| MotionDetection {
            supported: true,
            controllable: true,
            enabled: Some(enabled),
            error: None,
        },
    )
}

pub(super) fn set_motion(
    state: &ServerState,
    camera_id: &str,
    enabled: bool,
) -> Result<MotionDetection, ControlCommandError> {
    let camera = state.camera(camera_id).ok_or_else(|| {
        ControlCommandError::new(proto::ErrorCode::NotFound, 404, "camera not found")
    })?;
    if camera.control.is_none() && hikvision_route(&camera).is_none() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "motion detection control is unavailable for this camera",
        ));
    }
    update_motion(&camera, enabled).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            "camera motion update failed; its outcome may be unknown",
        )
    })?;
    let actual = motion_enabled(&camera).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            "camera motion verification failed",
        )
    })?;
    if actual != enabled {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            502,
            format!("camera motion state was {actual} after requesting {enabled}"),
        ));
    }
    Ok(MotionDetection {
        supported: true,
        controllable: true,
        enabled: Some(actual),
        error: None,
    })
}

fn update_motion(camera: &CameraEntry, enabled: bool) -> anyhow::Result<()> {
    if let Some(route) = hikvision_route(camera) {
        let mut client = hikvision_client(camera, &route)?;
        let mut motion = client.query(&::isapi::management::Motion::query(route.channel)?)?;
        motion.set_enabled(enabled)?;
        let result = client.command(&motion.update(route.channel)?)?;
        anyhow::ensure!(
            !result.reboot_required(),
            "camera accepted motion change but requires a reboot"
        );
        return Ok(());
    }
    let control = camera
        .control
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("motion detection control is unavailable"))?;
    let mut client = ReolinkClient::new_with_http_port(control.ip, control.http_port);
    client.login(&control.username, &control.password)?;
    client.set_alarm(0, enabled)
}
