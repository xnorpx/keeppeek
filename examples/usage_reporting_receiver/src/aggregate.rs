//! Privacy-preserving public aggregation with k >= 10 threshold and complementary suppression.
//!
//! Every published cell counts distinct non-expired installations and never contains
//! installation UUIDs or raw per-installation rows.

use crate::retention::MAX_RETENTION_DAYS;
use crate::schema::{SCHEMA_VERSION_V1, UsageReportV1};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MIN_INSTALLATIONS_PER_CELL: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicAggregateDataset {
    pub schema_version: u32,
    pub generated_at_unix_secs: i64,
    pub min_installations_per_cell: u64,
    pub max_retention_days: u32,
    pub suppressed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_installations: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_compatibility_installations: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_adoption_installations: Option<u64>,
    pub dimensions: AggregateDimensions,
    pub combined_dimensions: CombinedAggregateDimensions,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateDimensions {
    pub keeppeek_version: BTreeMap<String, u64>,
    pub release_channel: BTreeMap<String, u64>,
    pub deployment_type: BTreeMap<String, u64>,
    pub os_family: BTreeMap<String, u64>,
    pub architecture: BTreeMap<String, u64>,
    pub cpu_vendor: BTreeMap<String, u64>,
    pub cpu_model_family: BTreeMap<String, u64>,
    pub memory_bucket: BTreeMap<String, u64>,
    pub video_acceleration: BTreeMap<String, u64>,
    pub camera_manufacturer: BTreeMap<String, u64>,
    pub camera_model: BTreeMap<String, u64>,
    pub camera_backend: BTreeMap<String, u64>,
    pub camera_transport: BTreeMap<String, u64>,
    pub video_codec: BTreeMap<String, u64>,
    pub resolution_bucket: BTreeMap<String, u64>,
    pub camera_capability: BTreeMap<String, u64>,
    pub recording_mode: BTreeMap<String, u64>,
    pub storage_capacity_bucket: BTreeMap<String, u64>,
    pub projected_retention_bucket: BTreeMap<String, u64>,
    pub enabled_integration: BTreeMap<String, u64>,
    pub event_source_type: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombinedAggregateDimensions {
    pub os_x_architecture: BTreeMap<String, u64>,
    pub manufacturer_x_backend: BTreeMap<String, u64>,
    pub codec_x_resolution: BTreeMap<String, u64>,
}

/// Computes an aggregate-only public dataset from active non-expired reports.
pub fn compute_public_aggregates(
    reports: &[UsageReportV1],
    generated_at_unix_secs: i64,
) -> PublicAggregateDataset {
    let mut distinct_reports: BTreeMap<&str, &UsageReportV1> = BTreeMap::new();
    for report in reports {
        distinct_reports.insert(report.installation_id.as_str(), report);
    }

    let total_active = u64::try_from(distinct_reports.len()).unwrap_or(u64::MAX);
    if total_active < MIN_INSTALLATIONS_PER_CELL {
        return PublicAggregateDataset {
            schema_version: SCHEMA_VERSION_V1,
            generated_at_unix_secs,
            min_installations_per_cell: MIN_INSTALLATIONS_PER_CELL,
            max_retention_days: MAX_RETENTION_DAYS,
            suppressed: true,
            active_installations: None,
            camera_compatibility_installations: None,
            feature_adoption_installations: None,
            dimensions: AggregateDimensions::default(),
            combined_dimensions: CombinedAggregateDimensions::default(),
        };
    }

    let mut acc = Accumulator::default();
    for report in distinct_reports.values() {
        acc.observe(report);
    }

    let camera_total = publish_category_total(acc.camera_installations, total_active);
    let feature_total = publish_category_total(acc.feature_installations, total_active);
    let (dimensions, combined_dimensions) = build_dimensions(acc, total_active, feature_total);

    PublicAggregateDataset {
        schema_version: SCHEMA_VERSION_V1,
        generated_at_unix_secs,
        min_installations_per_cell: MIN_INSTALLATIONS_PER_CELL,
        max_retention_days: MAX_RETENTION_DAYS,
        suppressed: false,
        active_installations: Some(total_active),
        camera_compatibility_installations: camera_total,
        feature_adoption_installations: feature_total,
        dimensions,
        combined_dimensions,
    }
}

fn build_dimensions(
    acc: Accumulator,
    total_active: u64,
    feature_total: Option<u64>,
) -> (AggregateDimensions, CombinedAggregateDimensions) {
    let dimensions = AggregateDimensions {
        keeppeek_version: suppress_partitioned(acc.keeppeek_version, total_active),
        release_channel: suppress_partitioned(acc.release_channel, total_active),
        deployment_type: suppress_partitioned(acc.deployment_type, total_active),
        os_family: suppress_partitioned(acc.os_family, total_active),
        architecture: suppress_partitioned(acc.architecture, total_active),
        cpu_vendor: suppress_partitioned(acc.cpu_vendor, total_active),
        cpu_model_family: suppress_partitioned(acc.cpu_model_family, total_active),
        memory_bucket: suppress_partitioned(acc.memory_bucket, total_active),
        video_acceleration: suppress_independent(acc.video_acceleration),
        camera_manufacturer: suppress_independent(acc.camera_manufacturer),
        camera_model: suppress_independent(acc.camera_model),
        camera_backend: suppress_independent(acc.camera_backend),
        camera_transport: suppress_independent(acc.camera_transport),
        video_codec: suppress_independent(acc.video_codec),
        resolution_bucket: suppress_independent(acc.resolution_bucket),
        camera_capability: suppress_independent(acc.camera_capability),
        recording_mode: suppress_independent(acc.recording_mode),
        storage_capacity_bucket: feature_total
            .map(|total| suppress_partitioned(acc.storage_capacity_bucket, total))
            .unwrap_or_default(),
        projected_retention_bucket: feature_total
            .map(|total| suppress_partitioned(acc.projected_retention_bucket, total))
            .unwrap_or_default(),
        enabled_integration: suppress_independent(acc.enabled_integration),
        event_source_type: suppress_independent(acc.event_source_type),
    };
    let combined_dimensions = CombinedAggregateDimensions {
        os_x_architecture: suppress_partitioned(acc.os_x_architecture, total_active),
        manufacturer_x_backend: suppress_independent(acc.manufacturer_x_backend),
        codec_x_resolution: suppress_independent(acc.codec_x_resolution),
    };
    (dimensions, combined_dimensions)
}

/// Suppresses a category total if either the category or its non-opted-in complement is in 1..10.
const fn publish_category_total(category_count: u64, total_active: u64) -> Option<u64> {
    if category_count < MIN_INSTALLATIONS_PER_CELL {
        return None;
    }
    let complement = total_active.saturating_sub(category_count);
    if complement > 0 && complement < MIN_INSTALLATIONS_PER_CELL {
        return None;
    }
    Some(category_count)
}

/// Filters a mutually exclusive dimension so every published cell is >= 10 and any
/// non-zero suppressed residual against `parent_total` is also >= 10 (no differencing leak).
fn suppress_partitioned(counts: BTreeMap<String, u64>, parent_total: u64) -> BTreeMap<String, u64> {
    let mut kept: Vec<(String, u64)> = counts
        .into_iter()
        .filter(|(_, count)| *count >= MIN_INSTALLATIONS_PER_CELL)
        .collect();
    let mut kept_sum: u64 = kept.iter().map(|(_, count)| *count).sum();
    let mut residual = parent_total.saturating_sub(kept_sum);

    if residual > 0 && residual < MIN_INSTALLATIONS_PER_CELL {
        // Sort ascending by count (then deterministically by key) so we hide the smallest kept cells first.
        kept.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
        let mut drop_count = 0usize;
        while drop_count < kept.len() && residual < MIN_INSTALLATIONS_PER_CELL {
            let value = kept[drop_count].1;
            kept_sum = kept_sum.saturating_sub(value);
            residual = parent_total.saturating_sub(kept_sum);
            drop_count += 1;
        }
        kept.drain(0..drop_count);
    }

    kept.into_iter().collect()
}

/// Filters a multi-valued dimension where one installation may contribute to multiple cells.
fn suppress_independent(counts: BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    counts
        .into_iter()
        .filter(|(_, count)| *count >= MIN_INSTALLATIONS_PER_CELL)
        .collect()
}

#[derive(Default)]
struct Accumulator {
    camera_installations: u64,
    feature_installations: u64,
    keeppeek_version: BTreeMap<String, u64>,
    release_channel: BTreeMap<String, u64>,
    deployment_type: BTreeMap<String, u64>,
    os_family: BTreeMap<String, u64>,
    architecture: BTreeMap<String, u64>,
    cpu_vendor: BTreeMap<String, u64>,
    cpu_model_family: BTreeMap<String, u64>,
    memory_bucket: BTreeMap<String, u64>,
    video_acceleration: BTreeMap<String, u64>,
    camera_manufacturer: BTreeMap<String, u64>,
    camera_model: BTreeMap<String, u64>,
    camera_backend: BTreeMap<String, u64>,
    camera_transport: BTreeMap<String, u64>,
    video_codec: BTreeMap<String, u64>,
    resolution_bucket: BTreeMap<String, u64>,
    camera_capability: BTreeMap<String, u64>,
    recording_mode: BTreeMap<String, u64>,
    storage_capacity_bucket: BTreeMap<String, u64>,
    projected_retention_bucket: BTreeMap<String, u64>,
    enabled_integration: BTreeMap<String, u64>,
    event_source_type: BTreeMap<String, u64>,
    os_x_architecture: BTreeMap<String, u64>,
    manufacturer_x_backend: BTreeMap<String, u64>,
    codec_x_resolution: BTreeMap<String, u64>,
}

impl Accumulator {
    fn observe(&mut self, report: &UsageReportV1) {
        self.observe_basic(report);
        if let Some(camera) = &report.camera_compatibility {
            self.camera_installations += 1;
            self.observe_camera(camera);
        }
        if let Some(feature) = &report.feature_adoption {
            self.feature_installations += 1;
            self.observe_feature(feature);
        }
    }

    fn observe_basic(&mut self, report: &UsageReportV1) {
        let basic = &report.basic;
        increment(&mut self.keeppeek_version, basic.keeppeek_version.clone());
        let os = enum_slug(&basic.os_family);
        let arch = enum_slug(&basic.architecture);
        increment(&mut self.release_channel, enum_slug(&basic.release_channel));
        increment(&mut self.deployment_type, enum_slug(&basic.deployment_type));
        increment(&mut self.os_family, os.clone());
        increment(&mut self.architecture, arch.clone());
        increment(&mut self.os_x_architecture, format!("{os}:{arch}"));
        increment(&mut self.cpu_vendor, enum_slug(&basic.cpu_vendor));
        increment(
            &mut self.cpu_model_family,
            enum_slug(&basic.cpu_model_family),
        );
        increment(&mut self.memory_bucket, enum_slug(&basic.memory_bucket));

        let unique_accel: BTreeSet<String> =
            basic.video_acceleration.iter().map(enum_slug).collect();
        for accel in unique_accel {
            increment(&mut self.video_acceleration, accel);
        }
    }

    fn observe_camera(&mut self, camera: &crate::schema::CameraCompatibilityData) {
        let mut manufacturers = BTreeSet::new();
        let mut models = BTreeSet::new();
        let mut backends = BTreeSet::new();
        let mut transports = BTreeSet::new();
        let mut codecs = BTreeSet::new();
        let mut resolutions = BTreeSet::new();
        let mut capabilities = BTreeSet::new();
        let mut manufacturer_backend = BTreeSet::new();
        let mut codec_resolution = BTreeSet::new();

        for group in &camera.groups {
            let backend = enum_slug(&group.backend);
            let codec = enum_slug(&group.video_codec);
            let resolution = enum_slug(&group.resolution_bucket);
            manufacturers.insert(group.manufacturer.clone());
            models.insert(group.model.clone());
            backends.insert(backend.clone());
            transports.insert(enum_slug(&group.transport));
            codecs.insert(codec.clone());
            resolutions.insert(resolution.clone());
            manufacturer_backend.insert(format!("{}:{backend}", group.manufacturer));
            codec_resolution.insert(format!("{codec}:{resolution}"));
            for capability in &group.capabilities {
                capabilities.insert(enum_slug(capability));
            }
        }

        increment_all(&mut self.camera_manufacturer, manufacturers);
        increment_all(&mut self.camera_model, models);
        increment_all(&mut self.camera_backend, backends);
        increment_all(&mut self.camera_transport, transports);
        increment_all(&mut self.video_codec, codecs);
        increment_all(&mut self.resolution_bucket, resolutions);
        increment_all(&mut self.camera_capability, capabilities);
        increment_all(&mut self.manufacturer_x_backend, manufacturer_backend);
        increment_all(&mut self.codec_x_resolution, codec_resolution);
    }

    fn observe_feature(&mut self, feature: &crate::schema::FeatureAdoptionData) {
        increment(
            &mut self.storage_capacity_bucket,
            enum_slug(&feature.storage_capacity_bucket),
        );
        increment(
            &mut self.projected_retention_bucket,
            enum_slug(&feature.projected_retention_bucket),
        );

        let modes: BTreeSet<String> = feature
            .recording_modes
            .iter()
            .map(|entry| enum_slug(&entry.mode))
            .collect();
        let integrations: BTreeSet<String> =
            feature.enabled_integrations.iter().map(enum_slug).collect();
        let sources: BTreeSet<String> = feature.event_source_types.iter().map(enum_slug).collect();

        increment_all(&mut self.recording_mode, modes);
        increment_all(&mut self.enabled_integration, integrations);
        increment_all(&mut self.event_source_type, sources);
    }
}

fn increment(map: &mut BTreeMap<String, u64>, key: String) {
    *map.entry(key).or_insert(0) += 1;
}

fn increment_all(map: &mut BTreeMap<String, u64>, keys: BTreeSet<String>) {
    for key in keys {
        increment(map, key);
    }
}

fn enum_slug<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|val| val.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}
