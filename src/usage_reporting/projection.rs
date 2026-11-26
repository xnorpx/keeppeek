//! Snapshot-only projection from existing KeepPeek state into the allowlisted v1 DTO.
//!
//! Never probes cameras, storage, integrations, or the network, and never serializes
//! raw `CameraInfo`, `Config`, or `ServerHealthResponse` structures directly.

use super::model::{
    BasicSystemData, CURRENT_DISCLOSURE_REVISION, CameraCapabilityId, CameraCompatibilityData,
    CameraCompatibilityGroup, EventSourceTypeId, FeatureAdoptionData, IntegrationId,
    MAX_CAMERA_COUNT, MAX_CAMERA_GROUPS, MAX_LOGICAL_CORES, MAX_PHYSICAL_CORES, RecordingModeCount,
    SCHEMA_VERSION_V1, SchemaValidationError, UsageReportV1, VideoAcceleration,
};
use super::normalize::{
    normalize_architecture, normalize_camera_backend, normalize_camera_identity,
    normalize_camera_transport, normalize_cpu, normalize_deployment_type, normalize_memory_bucket,
    normalize_os_family, normalize_os_version, normalize_projected_retention_bucket,
    normalize_release_channel, normalize_resolution_bucket, normalize_storage_capacity_bucket,
    normalize_video_acceleration, normalize_video_codec,
};
use crate::api::CameraInfo;
use crate::camera_database::CameraDatabase;
use crate::config::Config;
use crate::health::ServerHealthResponse;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsentSelection {
    pub basic: bool,
    pub camera_compatibility: bool,
    pub feature_adoption: bool,
    pub consented_disclosure_revision: u32,
}

impl ConsentSelection {
    pub const fn from_config(
        config: &crate::config::usage_reporting::UsageReportingConfig,
    ) -> Self {
        Self {
            basic: config.basic,
            camera_compatibility: config.camera_compatibility,
            feature_adoption: config.feature_adoption,
            consented_disclosure_revision: config.consented_disclosure_revision,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProjectionHealthInput {
    pub version: String,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub architecture: String,
    pub cpu_brand: Option<String>,
    pub physical_cores: Option<usize>,
    pub logical_cores: usize,
    pub memory_total_bytes: u64,
    pub configured_cameras: usize,
    pub connected_cameras: usize,
    pub decodable_cameras: usize,
    pub recording_cameras: usize,
    pub long_term_max_bytes: u64,
}

impl ProjectionHealthInput {
    pub fn from_server_health(health: &ServerHealthResponse) -> Self {
        Self {
            version: health.version.to_owned(),
            os_name: health.system.os_name.clone(),
            os_version: health.system.os_version.clone(),
            architecture: health.system.architecture.to_owned(),
            cpu_brand: health.system.cpu_brand.clone(),
            physical_cores: health.system.physical_cores,
            logical_cores: health.system.logical_cores,
            memory_total_bytes: health.system.memory.total_bytes,
            configured_cameras: health.totals.configured_cameras,
            connected_cameras: health.totals.connected_cameras,
            decodable_cameras: health.totals.decodable_cameras,
            recording_cameras: health.totals.recording_cameras,
            long_term_max_bytes: health.storage.long_term_max_bytes,
        }
    }
}

pub struct ProjectionSnapshot<'a> {
    pub config: &'a Config,
    pub health: &'a ProjectionHealthInput,
    pub cameras: &'a [CameraInfo],
    pub camera_database: Option<&'a CameraDatabase>,
    pub estimated_retention_days: Option<f64>,
    pub is_container: bool,
    pub video_accelerations: &'a [&'a str],
    pub has_notifications_pushover: bool,
    pub has_notifications_webhook: bool,
    pub has_external_api_events: bool,
}

/// Projects an exact allowlisted `UsageReportV1` from already-available snapshots.
pub fn project_report(
    snapshot: &ProjectionSnapshot<'_>,
    selection: ConsentSelection,
    installation_id: &str,
    report_sequence: u64,
) -> Result<UsageReportV1, SchemaValidationError> {
    if !selection.basic {
        return Err(SchemaValidationError::UnsupportedDisclosureRevision);
    }
    let disclosure_revision = selection
        .consented_disclosure_revision
        .clamp(1, CURRENT_DISCLOSURE_REVISION);

    let basic = project_basic(snapshot);
    let camera_compatibility = selection
        .camera_compatibility
        .then(|| project_camera_compatibility(snapshot))
        .transpose()?;
    let feature_adoption = selection
        .feature_adoption
        .then(|| project_feature_adoption(snapshot));

    let report = UsageReportV1 {
        schema_version: SCHEMA_VERSION_V1,
        disclosure_revision,
        report_sequence: report_sequence.max(1),
        installation_id: installation_id.to_owned(),
        basic,
        camera_compatibility,
        feature_adoption,
    };
    report.validate()?;
    Ok(report)
}

fn project_basic(snapshot: &ProjectionSnapshot<'_>) -> BasicSystemData {
    let health = snapshot.health;
    let version = sanitize_semver(&health.version);
    let release_channel = normalize_release_channel(&version);
    let (cpu_vendor, cpu_model_family) = normalize_cpu(health.cpu_brand.as_deref());
    let physical_cores = health
        .physical_cores
        .and_then(|count| u32::try_from(count).ok())
        .filter(|count| *count >= 1)
        .map(|count| count.min(MAX_PHYSICAL_CORES));
    let logical_cores = u32::try_from(health.logical_cores)
        .unwrap_or(MAX_LOGICAL_CORES)
        .clamp(1, MAX_LOGICAL_CORES);
    let os_family = health.os_name.as_deref().map_or_else(
        || normalize_os_family(std::env::consts::OS),
        normalize_os_family,
    );

    let mut accelerations = BTreeSet::new();
    for raw in snapshot.video_accelerations {
        accelerations.insert(normalize_video_acceleration(raw));
    }
    if accelerations.is_empty() {
        accelerations.insert(VideoAcceleration::None);
    }

    BasicSystemData {
        keeppeek_version: version,
        release_channel,
        deployment_type: normalize_deployment_type(snapshot.is_container),
        os_family,
        os_version: normalize_os_version(health.os_version.as_deref()),
        architecture: normalize_architecture(&health.architecture),
        cpu_vendor,
        cpu_model_family,
        physical_cores,
        logical_cores,
        memory_bucket: normalize_memory_bucket(health.memory_total_bytes),
        video_acceleration: accelerations.into_iter().take(8).collect(),
    }
}

fn project_camera_compatibility(
    snapshot: &ProjectionSnapshot<'_>,
) -> Result<CameraCompatibilityData, SchemaValidationError> {
    let health = snapshot.health;
    let mut grouped: BTreeMap<CameraGroupKey, u32> = BTreeMap::new();

    for camera in snapshot.cameras {
        let (manufacturer, model) = normalize_camera_identity(
            snapshot.camera_database,
            camera.manufacturer.as_deref(),
            camera.model.as_deref(),
        );
        let backend = normalize_camera_backend(&camera.backend);
        let transport = normalize_camera_transport(&camera.transport);
        let primary_profile = camera
            .profiles
            .iter()
            .find(|profile| profile.stream.eq_ignore_ascii_case("main"))
            .or_else(|| camera.profiles.first());
        let video_codec =
            normalize_video_codec(primary_profile.and_then(|profile| profile.encoding.as_deref()));
        let resolution_bucket = normalize_resolution_bucket(
            primary_profile.and_then(|profile| profile.resolution.as_deref()),
        );
        let capabilities = extract_camera_capabilities(camera);
        let key = CameraGroupKey {
            manufacturer,
            model,
            backend,
            transport,
            video_codec,
            resolution_bucket,
            capabilities,
        };
        let entry = grouped.entry(key).or_insert(0);
        *entry = entry.saturating_add(1).min(MAX_CAMERA_COUNT);
    }

    if grouped.len() > MAX_CAMERA_GROUPS {
        return Err(SchemaValidationError::TooManyCameraGroups);
    }

    let groups = grouped
        .into_iter()
        .map(|(key, count)| CameraCompatibilityGroup {
            manufacturer: key.manufacturer.to_owned(),
            model: key.model.to_owned(),
            backend: key.backend,
            transport: key.transport,
            video_codec: key.video_codec,
            resolution_bucket: key.resolution_bucket,
            capabilities: key.capabilities,
            count,
        })
        .collect();

    Ok(CameraCompatibilityData {
        configured_cameras: clamp_camera_count(health.configured_cameras),
        connected_cameras: clamp_camera_count(health.connected_cameras),
        decodable_cameras: clamp_camera_count(health.decodable_cameras),
        recording_cameras: clamp_camera_count(health.recording_cameras),
        groups,
    })
}

fn project_feature_adoption(snapshot: &ProjectionSnapshot<'_>) -> FeatureAdoptionData {
    let mut mode_counts = BTreeMap::new();
    let configured_cameras = snapshot.config.camera_configurations().unwrap_or_default();
    let mut has_generic_motion = false;
    let mut has_reolink_camera = snapshot.cameras.iter().any(|camera| camera.is_reolink);
    let mut has_hikvision_camera = false;

    for (section, entries) in &configured_cameras {
        if section.eq_ignore_ascii_case("reolink") && !entries.is_empty() {
            has_reolink_camera = true;
        }
        if section.eq_ignore_ascii_case("hikvision") && !entries.is_empty() {
            has_hikvision_camera = true;
        }
        for camera_cfg in entries {
            if camera_cfg.record_generic_motion_events {
                has_generic_motion = true;
            }
            let normalized = map_camera_recording_mode(camera_cfg.recording_mode);
            let entry = mode_counts.entry(normalized).or_insert(0u32);
            *entry = entry.saturating_add(1).min(MAX_CAMERA_COUNT);
        }
    }
    let recording_modes = mode_counts
        .into_iter()
        .map(|(mode, count)| RecordingModeCount { mode, count })
        .collect();

    let storage_bytes = snapshot.health.long_term_max_bytes;
    let enabled_integrations = collect_integrations(snapshot);
    let event_source_types = collect_event_sources(
        snapshot,
        has_reolink_camera,
        has_hikvision_camera,
        has_generic_motion,
    );

    FeatureAdoptionData {
        recording_modes,
        storage_capacity_bucket: normalize_storage_capacity_bucket(storage_bytes),
        projected_retention_bucket: normalize_projected_retention_bucket(
            snapshot.estimated_retention_days,
        ),
        enabled_integrations,
        event_source_types,
    }
}

const fn map_camera_recording_mode(
    mode: crate::cameras::CameraRecordingMode,
) -> super::model::TelemetryRecordingMode {
    match mode {
        crate::cameras::CameraRecordingMode::Off => super::model::TelemetryRecordingMode::Disabled,
        crate::cameras::CameraRecordingMode::EventOnly => {
            super::model::TelemetryRecordingMode::EventOnly
        }
        crate::cameras::CameraRecordingMode::Sub
        | crate::cameras::CameraRecordingMode::Main
        | crate::cameras::CameraRecordingMode::Both
        | crate::cameras::CameraRecordingMode::EventBoost => {
            super::model::TelemetryRecordingMode::Continuous
        }
    }
}

fn collect_integrations(snapshot: &ProjectionSnapshot<'_>) -> Vec<IntegrationId> {
    let cfg = snapshot.config;
    let mut items = BTreeSet::new();
    if cfg.event_forwarder.mqtt.enabled {
        items.insert(IntegrationId::MqttForwarder);
    }
    if !cfg.direct_card.allowed_origins.is_empty() {
        items.insert(IntegrationId::HomeAssistantCard);
    }
    if snapshot.has_notifications_pushover {
        items.insert(IntegrationId::NotificationsPushover);
    }
    if snapshot.has_notifications_webhook {
        items.insert(IntegrationId::NotificationsWebhook);
    }
    if let Some(ext) = &cfg.external_auth {
        for provider in &ext.providers {
            match provider.method {
                crate::access::external::Method::Oidc(_) => {
                    items.insert(IntegrationId::ExternalAuthOidc);
                }
                crate::access::external::Method::Proxy(_) => {
                    items.insert(IntegrationId::ExternalAuthProxy);
                }
            }
        }
    }
    if cfg.sharing.enabled {
        items.insert(IntegrationId::GuestSharing);
    }
    if cfg.storage.archive.is_some() {
        items.insert(IntegrationId::LocalArchive);
    }
    if cfg.storage.named_volumes.is_some() {
        items.insert(IntegrationId::NamedVolumes);
    }
    if cfg.reporting.is_some() {
        items.insert(IntegrationId::BugReporting);
    }
    if cfg.battery_wake.enabled {
        items.insert(IntegrationId::BatteryWake);
    }
    if cfg.privacy.default.is_some() || !cfg.privacy.cameras.is_empty() {
        items.insert(IntegrationId::PrivacySchedules);
    }
    if !cfg.runtime_profiles.is_empty() {
        items.insert(IntegrationId::RuntimeProfiles);
    }
    items.into_iter().collect()
}

fn collect_event_sources(
    snapshot: &ProjectionSnapshot<'_>,
    has_reolink_camera: bool,
    has_hikvision_camera: bool,
    has_generic_motion: bool,
) -> Vec<EventSourceTypeId> {
    let mut sources = BTreeSet::new();
    let cfg = snapshot.config;
    if has_reolink_camera {
        sources.insert(EventSourceTypeId::ReolinkEvents);
    }
    if has_hikvision_camera
        || cfg
            .isapi_callbacks
            .as_ref()
            .is_some_and(|callbacks| !callbacks.sources.is_empty())
    {
        sources.insert(EventSourceTypeId::HikvisionIsapi);
    }
    if snapshot
        .cameras
        .iter()
        .any(|camera| !camera.is_reolink && camera.capabilities.events)
    {
        sources.insert(EventSourceTypeId::OnvifEvents);
    }
    if has_generic_motion {
        sources.insert(EventSourceTypeId::GenericMotion);
    }
    if snapshot.has_external_api_events {
        sources.insert(EventSourceTypeId::ExternalApiEvents);
    }
    if !cfg.operational_events.cameras.is_empty() || cfg.operational_events.record_short_flaps {
        sources.insert(EventSourceTypeId::OperationalEvents);
    }
    sources.into_iter().collect()
}

fn extract_camera_capabilities(camera: &CameraInfo) -> Vec<CameraCapabilityId> {
    let mut caps = Vec::new();
    let c = &camera.capabilities;
    if c.ptz {
        caps.push(CameraCapabilityId::Ptz);
    }
    if c.audio {
        caps.push(CameraCapabilityId::Audio);
    }
    if c.events {
        caps.push(CameraCapabilityId::Events);
    }
    if c.recording {
        caps.push(CameraCapabilityId::Recording);
    }
    if c.analytics {
        caps.push(CameraCapabilityId::Analytics);
    }
    if c.imaging {
        caps.push(CameraCapabilityId::Imaging);
    }
    if c.two_way_audio {
        caps.push(CameraCapabilityId::TwoWayAudio);
    }
    if !c.quick_replies.is_empty() {
        caps.push(CameraCapabilityId::QuickReplies);
    }
    if !c.chimes.is_empty() {
        caps.push(CameraCapabilityId::Chimes);
    }
    caps.sort_unstable();
    caps
}

fn clamp_camera_count(count: usize) -> u32 {
    u32::try_from(count)
        .unwrap_or(MAX_CAMERA_COUNT)
        .min(MAX_CAMERA_COUNT)
}

fn sanitize_semver(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 5 && trimmed.len() <= 64 {
        trimmed.to_owned()
    } else {
        "0.0.0".to_owned()
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct CameraGroupKey {
    manufacturer: &'static str,
    model: &'static str,
    backend: super::model::TelemetryCameraBackend,
    transport: super::model::TelemetryCameraTransport,
    video_codec: super::model::VideoCodec,
    resolution_bucket: super::model::ResolutionBucket,
    capabilities: Vec<CameraCapabilityId>,
}

/// Detects whether KeepPeek is running inside a container without performing network probes.
pub fn detect_container_runtime() -> bool {
    Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists()
}

use std::path::Path;
