use std::{collections::HashMap, net::IpAddr};

use prometheus_client::{
    encoding::EncodeLabelSet,
    metrics::{counter::Counter, family::Family, gauge::Gauge},
    registry::Registry,
};

use crate::{camera_events::Evidence, health::ServerHealthResponse, stats::CameraHealthReport};

use super::saturating_i64;

type CounterSpec = (&'static str, &'static str, fn(&Evidence) -> u64);
type GaugeSpec = (&'static str, &'static str, fn(&Evidence) -> Option<i64>);

const COUNTERS: [CounterSpec; 17] = [
    (
        "camera_events_pulls",
        "Cumulative successful ONVIF event pulls",
        |evidence| evidence.pulls,
    ),
    (
        "camera_events_empty_pulls",
        "Cumulative ONVIF event pulls without notifications",
        |evidence| evidence.empty_pulls,
    ),
    (
        "camera_events_notifications",
        "Cumulative camera event notifications",
        |evidence| evidence.notifications,
    ),
    (
        "camera_events_parse_errors",
        "Cumulative camera event parse errors",
        |evidence| evidence.parse_errors,
    ),
    (
        "camera_events_reconnects",
        "Cumulative ONVIF event subscription reconnects",
        |evidence| evidence.reconnects,
    ),
    (
        "camera_events_renewals",
        "Cumulative successful ONVIF event subscription renewals",
        |evidence| evidence.renewals,
    ),
    (
        "camera_events_renew_failures",
        "Cumulative failed ONVIF event subscription renewals",
        |evidence| evidence.renew_failures,
    ),
    (
        "camera_events_deduplicated",
        "Cumulative duplicate camera events suppressed",
        |evidence| evidence.deduplicated,
    ),
    (
        "camera_events_metadata_bytes",
        "Cumulative camera event metadata bytes",
        |evidence| evidence.metadata_bytes,
    ),
    (
        "camera_events_metadata_documents",
        "Cumulative camera event metadata documents",
        |evidence| evidence.metadata_documents,
    ),
    (
        "camera_events_metadata_loss",
        "Cumulative camera event metadata loss",
        |evidence| evidence.metadata_loss,
    ),
    (
        "camera_events_metadata_errors",
        "Cumulative camera event metadata errors",
        |evidence| evidence.metadata_errors,
    ),
    (
        "camera_events_queue_drops",
        "Cumulative camera event queue drops",
        |evidence| evidence.queue_drops,
    ),
    (
        "camera_events_delivery_stalls",
        "Cumulative camera event delivery stalls",
        |evidence| evidence.delivery_stalls,
    ),
    (
        "camera_events_dropped",
        "Cumulative camera event transitions dropped",
        |evidence| evidence.dropped,
    ),
    (
        "camera_events_snapshots",
        "Cumulative successful camera event snapshots",
        |evidence| evidence.snapshots,
    ),
    (
        "camera_events_snapshot_failures",
        "Cumulative camera event snapshot failures",
        |evidence| evidence.snapshot_failures,
    ),
];

const GAUGES: [GaugeSpec; 6] = [
    (
        "camera_events_pull_advertised",
        "Whether discovery advertised ONVIF pull support; omitted when unknown",
        |evidence| evidence.pull_advertised.map(i64::from),
    ),
    (
        "camera_events_pull_capable",
        "Whether ONVIF event pulls have demonstrated working capability",
        |evidence| Some(i64::from(evidence.pull_capable)),
    ),
    (
        "camera_events_unsubscribed",
        "Whether the ONVIF event subscription was unsubscribed successfully",
        |evidence| Some(i64::from(evidence.unsubscribed)),
    ),
    (
        "camera_events_lease_milliseconds",
        "Remaining ONVIF event subscription lease in milliseconds",
        |evidence| Some(saturating_i64(evidence.lease_ms)),
    ),
    (
        "camera_events_active",
        "Current active camera events",
        |evidence| Some(saturating_i64(evidence.active)),
    ),
    (
        "camera_events_metadata_available",
        "Whether camera event metadata has been observed",
        |evidence| Some(i64::from(evidence.metadata_available)),
    ),
];

const MODES: [&str; 4] = ["onvif-pullpoint", "rtsp-metadata", "vendor", "unknown"];
const STATES: [&str; 12] = [
    "starting",
    "discovered",
    "discovering",
    "observed",
    "subscribed",
    "reconnecting",
    "stopped",
    "commit_unknown",
    "delivery-stalled",
    "authentication-failed",
    "unsupported",
    "unknown",
];

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Labels {
    camera_id: String,
    camera_name: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ModeLabels {
    camera_id: String,
    camera_name: String,
    mode: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct StateLabels {
    camera_id: String,
    camera_name: String,
    state: &'static str,
}

struct Camera<'a> {
    labels: Labels,
    evidence: &'a Evidence,
}

pub(super) fn register(
    registry: &mut Registry,
    health: &ServerHealthResponse,
    reports: &[CameraHealthReport],
) {
    let evidence = reports
        .iter()
        .filter_map(|report| report.events.as_ref().map(|events| (report.ip, events)))
        .collect::<HashMap<_, _>>();
    let cameras = health
        .cameras
        .iter()
        .filter_map(|camera| {
            let ip = camera.ip.parse::<IpAddr>().ok()?;
            let evidence = evidence.get(&ip).copied()?;
            Some(Camera {
                labels: Labels {
                    camera_id: camera.id.clone(),
                    camera_name: camera.name.clone(),
                },
                evidence,
            })
        })
        .collect::<Vec<_>>();
    if cameras.is_empty() {
        return;
    }
    register_counters(registry, &cameras);
    register_gauges(registry, &cameras);
    register_states(registry, &cameras);
}

fn register_counters(registry: &mut Registry, cameras: &[Camera<'_>]) {
    for (name, help, value) in COUNTERS {
        let family = Family::<Labels, Counter>::default();
        for camera in cameras {
            family
                .get_or_create(&camera.labels)
                .inc_by(value(camera.evidence));
        }
        registry.register(name, help, family);
    }
}

fn register_gauges(registry: &mut Registry, cameras: &[Camera<'_>]) {
    for (name, help, value) in GAUGES {
        let family = Family::<Labels, Gauge>::default();
        for camera in cameras {
            if let Some(value) = value(camera.evidence) {
                family.get_or_create(&camera.labels).set(value);
            }
        }
        registry.register(name, help, family);
    }
}

fn register_states(registry: &mut Registry, cameras: &[Camera<'_>]) {
    let modes = Family::<ModeLabels, Gauge>::default();
    let states = Family::<StateLabels, Gauge>::default();
    for camera in cameras {
        let mode = enumeration(camera.evidence.mode, &MODES);
        for candidate in MODES {
            modes
                .get_or_create(&ModeLabels {
                    camera_id: camera.labels.camera_id.clone(),
                    camera_name: camera.labels.camera_name.clone(),
                    mode: candidate,
                })
                .set(i64::from(mode == candidate));
        }
        let state = enumeration(camera.evidence.state, &STATES);
        for candidate in STATES {
            states
                .get_or_create(&StateLabels {
                    camera_id: camera.labels.camera_id.clone(),
                    camera_name: camera.labels.camera_name.clone(),
                    state: candidate,
                })
                .set(i64::from(state == candidate));
        }
    }
    registry.register(
        "camera_events_mode",
        "Current camera event transport mode as a fixed one-hot enumeration",
        modes,
    );
    registry.register(
        "camera_events_state",
        "Current camera event runtime state as a fixed one-hot enumeration",
        states,
    );
}

fn enumeration(value: &str, candidates: &[&'static str]) -> &'static str {
    candidates
        .iter()
        .copied()
        .find(|candidate| *candidate == value)
        .unwrap_or("unknown")
}
