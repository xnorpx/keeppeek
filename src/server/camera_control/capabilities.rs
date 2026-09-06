use std::net::IpAddr;
use std::sync::Arc;

use ::isapi::management::{AudioChannel, DeviceInfo, Motion, PtzCapabilities, Query};

use super::{CameraEntry, ServerState, hikvision_client, hikvision_route, proto};

#[derive(Clone, Default)]
pub(in crate::server) struct Report {
    pub ptz: Option<PtzCapabilities>,
    device: Option<DeviceInfo>,
    audio: Option<Vec<AudioChannel>>,
    motion: Option<bool>,
    inherited_audio: bool,
}

impl ServerState {
    pub(in crate::server) fn probe_hikvision_capabilities(&self, ip: IpAddr) {
        let Some(camera) = self.camera(&ip.to_string()) else {
            return;
        };
        let Some(route) = hikvision_route(&camera) else {
            return;
        };
        let mut client = match hikvision_client(&camera, &route) {
            Ok(client) => client,
            Err(error) => {
                tracing::debug!(%ip, %error, "ISAPI capability client is unavailable");
                return;
            }
        };
        let report = Report {
            device: probe(&mut client, DeviceInfo::query(), ip, "device"),
            ptz: probe(
                &mut client,
                PtzCapabilities::query(route.channel),
                ip,
                "PTZ",
            ),
            audio: probe(&mut client, AudioChannel::list(), ip, "audio"),
            motion: probe(&mut client, Motion::query(route.channel), ip, "motion")
                .map(|motion| motion.enabled()),
            inherited_audio: false,
        };
        self.apply_hikvision_capabilities(&camera, route.channel, report);
    }

    pub(in crate::server) fn apply_hikvision_capabilities(
        &self,
        previous: &CameraEntry,
        channel: u32,
        mut report: Report,
    ) {
        let mut cameras = self
            .cameras
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(camera) = cameras
            .iter_mut()
            .find(|camera| camera.info.id == previous.info.id)
        else {
            return;
        };
        if !Arc::ptr_eq(&camera.control_revision, &previous.control_revision) {
            return;
        }
        report.inherited_audio = camera
            .hikvision
            .as_ref()
            .map_or(camera.info.capabilities.audio, |cached| {
                cached.inherited_audio
            })
            || camera
                .info
                .profiles
                .iter()
                .any(|profile| profile.audio.is_some());
        if let Some(cached) = &camera.hikvision {
            if report.ptz.is_none() {
                report.ptz.clone_from(&cached.ptz);
            }
            if report.audio.is_none() {
                report.audio.clone_from(&cached.audio);
            }
            if report.device.is_none() {
                report.device.clone_from(&cached.device);
            }
            report.motion = report.motion.or(cached.motion);
        }
        if let Some(device) = &report.device {
            camera.info.model = Some(device.model().to_owned());
            if let Some(serial) = device.serial() {
                camera.info.serial_number = Some(serial.to_owned());
            }
            if let Some(firmware) = device.firmware() {
                camera.info.firmware_version = Some(firmware.to_owned());
            }
        }
        camera.info.capabilities.ptz = report.ptz.as_ref().is_some_and(|ptz| {
            ptz.continuous_pan_tilt() || ptz.continuous_zoom() || ptz.max_presets() > 0
        });
        camera.info.capabilities.events |= report.motion.is_some();
        camera.info.capabilities.two_way_audio = report.audio.as_ref().is_some_and(|channels| {
            channels
                .iter()
                .any(|audio| associated(audio, channel) && audio.speaker_supported())
        });
        camera.info.capabilities.audio = report.inherited_audio
            || report.audio.as_ref().is_some_and(|channels| {
                channels
                    .iter()
                    .any(|audio| associated(audio, channel) && audio.microphone_supported())
            });
        camera.hikvision = Some(report);
    }
}

fn associated(audio: &AudioChannel, channel: u32) -> bool {
    if audio.video_inputs().is_empty() {
        audio.id() == channel
    } else {
        audio.video_inputs().contains(&channel)
    }
}

fn probe<Output>(
    client: &mut ::isapi::blocking::Client,
    query: Result<Query<Output>, ::isapi::Error>,
    ip: IpAddr,
    resource: &str,
) -> Option<Output> {
    match query.and_then(|query| client.query(&query)) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::debug!(%ip, %error, resource, "ISAPI capability is not verified");
            None
        }
    }
}

pub(in crate::server) fn ptz_capability(camera: &CameraEntry) -> proto::PtzCapability {
    if hikvision_route(camera).is_some() {
        let Some(ptz) = camera
            .hikvision
            .as_ref()
            .and_then(|report| report.ptz.as_ref())
        else {
            return proto::PtzCapability::default();
        };
        let continuous = ptz.continuous_pan_tilt() || ptz.continuous_zoom();
        let presets = ptz.max_presets() > 0;
        return proto::PtzCapability {
            supported: continuous || presets,
            continuous,
            zoom: ptz.continuous_zoom(),
            presets,
            relative: false,
        };
    }
    let supported = camera.info.capabilities.ptz && camera.control.is_some();
    proto::PtzCapability {
        supported,
        continuous: supported,
        zoom: supported,
        presets: supported,
        relative: false,
    }
}
