use crate::{
    event_forwarder::{MqttConnectionState, MqttStatus},
    health::ServerHealthResponse,
    server::recording_coverage::RecordingCoverageMetricSnapshot,
};
use prometheus_client::{
    encoding::{EncodeLabelSet, text::encode_registry},
    metrics::{counter::Counter, family::Family, gauge::Gauge},
    registry::Registry,
};
use std::sync::atomic::AtomicU64;

type FloatGauge = Gauge<f64, AtomicU64>;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ServerInfoLabels {
    version: String,
    status: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct CameraStateLabels {
    camera_id: String,
    camera_name: String,
    state: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct CameraStreamLabels {
    camera_id: String,
    camera_name: String,
    stream: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RecordingWriterLabels {
    camera_id: String,
    camera_name: String,
    stream: String,
    state: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct CameraDimensionLabels {
    camera_id: String,
    camera_name: String,
    dimension: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct CameraStreamDimensionLabels {
    camera_id: String,
    camera_name: String,
    stream: String,
    dimension: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct DiskLabels {
    name: String,
    mount_point: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct SeverityLabels {
    severity: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct OperationalEventLabels {
    camera_id: String,
    stream: String,
    kind: String,
    severity: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct WebRtcSourceLabels {
    camera_ip: String,
    stream: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct QuantileLabels {
    quantile: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessMetricsSnapshot {
    pub(crate) authentication_successes: u64,
    pub(crate) authentication_failures: u64,
    pub(crate) authorization_denials: u64,
    pub(crate) sessions_created: u64,
    pub(crate) sessions_revoked_or_expired: u64,
    pub(crate) active_sessions: u64,
    pub(crate) active_credentials: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExternalAnalysisMetricsSnapshot {
    pub(crate) sessions_active: u64,
    pub(crate) media_subscriptions_active: u64,
    pub(crate) event_subscriptions_active: u64,
    pub(crate) event_subscription_starts: u64,
    pub(crate) event_subscription_rejections: u64,
    pub(crate) event_subscription_deliveries: u64,
    pub(crate) event_subscription_sheds: u64,
    pub(crate) event_delivery_queue_depth: u64,
    pub(crate) event_delivery_queue_depth_high_water: u64,
    pub(crate) event_delivery_pending_bytes: u64,
    pub(crate) event_delivery_pending_bytes_high_water: u64,
    pub(crate) event_deliveries_queued: u64,
    pub(crate) event_delivery_drops: u64,
    pub(crate) event_publications_active: u64,
    pub(crate) event_publication_staged_bytes: u64,
    pub(crate) event_publication_starts: u64,
    pub(crate) event_publication_commits: u64,
    pub(crate) event_publication_aborts: u64,
    pub(crate) event_publication_expirations: u64,
    pub(crate) event_publication_rejections: u64,
    pub(crate) event_publication_storage_failures: u64,
    pub(crate) event_publication_commit_latency_ms_p50: u64,
    pub(crate) event_publication_commit_latency_ms_p95: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackupMetricsSnapshot {
    pub(crate) operation_successes: u64,
    pub(crate) operation_failures: u64,
    pub(crate) retained_backups: u64,
    pub(crate) retained_archive_bytes: u64,
    pub(crate) active_restore: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NotificationMetricsSnapshot {
    pub(crate) configured_rules: u64,
    pub(crate) pending_deliveries: u64,
    pub(crate) candidates_accepted: u64,
    pub(crate) candidates_dropped: u64,
    pub(crate) notifications_created: u64,
    pub(crate) notifications_replaced: u64,
    pub(crate) notifications_suppressed: u64,
    pub(crate) delivery_attempts: u64,
    pub(crate) delivery_retries: u64,
    pub(crate) delivery_successes: u64,
    pub(crate) delivery_failures: u64,
}

pub fn encode_health_metrics(
    health: &ServerHealthResponse,
    access: Option<AccessMetricsSnapshot>,
    recording: Option<&RecordingCoverageMetricSnapshot>,
    backup: Option<BackupMetricsSnapshot>,
    notifications: Option<NotificationMetricsSnapshot>,
    mqtt: Option<&MqttStatus>,
    external_analysis: Option<ExternalAnalysisMetricsSnapshot>,
) -> Result<String, std::fmt::Error> {
    let mut registry = Registry::with_prefix("keeppeek");

    let server_info = Family::<ServerInfoLabels, Gauge>::default();
    server_info
        .get_or_create(&ServerInfoLabels {
            version: health.version.to_owned(),
            status: health.status.clone(),
        })
        .set(1);
    registry.register(
        "server_info",
        "Static server build and current health status",
        server_info,
    );
    register_gauge(
        &mut registry,
        "server_uptime_seconds",
        "Elapsed KeepPeek server runtime",
        health.uptime_seconds,
    );

    register_gauge(
        &mut registry,
        "cameras_configured",
        "Configured camera count",
        health.totals.configured_cameras,
    );
    register_gauge(
        &mut registry,
        "cameras_connected",
        "Configured cameras with all expected transports connected",
        health.totals.connected_cameras,
    );
    register_gauge(
        &mut registry,
        "cameras_fresh",
        "Configured cameras with fresh frames on every expected video stream",
        health.totals.fresh_cameras,
    );
    register_gauge(
        &mut registry,
        "cameras_decodable",
        "Configured cameras with recent keyframes on every expected video stream",
        health.totals.decodable_cameras,
    );
    register_gauge(
        &mut registry,
        "cameras_recording_requested",
        "Configured cameras with one or more requested recording streams",
        health.totals.recording_requested_cameras,
    );
    register_gauge(
        &mut registry,
        "cameras_recording",
        "Configured cameras whose requested recording streams are progressing",
        health.totals.recording_cameras,
    );
    register_gauge(
        &mut registry,
        "cameras_unknown",
        "Configured cameras with insufficient evidence for a canonical state",
        health.totals.unknown_cameras,
    );
    register_gauge(
        &mut registry,
        "video_streams_configured",
        "Configured video stream count",
        health.totals.configured_video_streams,
    );
    register_gauge(
        &mut registry,
        "video_streams_connected",
        "Expected video streams with connected transports",
        health.totals.connected_video_streams,
    );
    register_gauge(
        &mut registry,
        "video_streams_fresh",
        "Expected video streams with fresh frames",
        health.totals.fresh_video_streams,
    );
    register_gauge(
        &mut registry,
        "video_streams_decodable",
        "Expected video streams with recent keyframes",
        health.totals.decodable_video_streams,
    );
    register_gauge(
        &mut registry,
        "video_streams_recording_requested",
        "Expected video streams requested by camera recording policy",
        health.totals.recording_requested_video_streams,
    );
    register_gauge(
        &mut registry,
        "video_streams_recording",
        "Requested video streams with current recording progress",
        health.totals.recording_video_streams,
    );
    register_float_gauge(
        &mut registry,
        "ingress_frames_per_second",
        "Aggregate current ingress video frame rate",
        health.totals.ingress_fps,
    );
    register_gauge(
        &mut registry,
        "ingress_bitrate_bits_per_second",
        "Aggregate current ingress video bitrate",
        health.totals.ingress_bitrate_bps,
    );
    register_counter(
        &mut registry,
        "ingress_frames",
        "Cumulative ingress video frames",
        health.totals.frames,
    );
    register_counter(
        &mut registry,
        "ingress_keyframes",
        "Cumulative ingress video keyframes",
        health.totals.keyframes,
    );
    register_counter(
        &mut registry,
        "ingress_drops",
        "Cumulative ingress frame drops",
        health.totals.drops,
    );
    register_counter(
        &mut registry,
        "ingress_errors",
        "Cumulative ingress errors",
        health.totals.errors,
    );
    register_counter(
        &mut registry,
        "ingress_reconnects",
        "Cumulative camera stream reconnects",
        health.totals.reconnects,
    );

    register_float_gauge(
        &mut registry,
        "system_cpu_usage_ratio",
        "Current host CPU utilization ratio",
        f64::from(health.system.system_cpu_percent) / 100.0,
    );
    if let Some(value) = health.system.process.cpu_capacity_percent {
        register_float_gauge(
            &mut registry,
            "process_cpu_usage_ratio",
            "Current KeepPeek CPU utilization ratio across host capacity",
            f64::from(value) / 100.0,
        );
    }
    if let Some(value) = health.system.process.cpu_core_equivalents {
        register_float_gauge(
            &mut registry,
            "process_cpu_core_equivalents",
            "Logical CPU cores currently consumed by KeepPeek",
            f64::from(value),
        );
    }
    if let Some(value) = health.system.process.resident_memory_bytes {
        register_gauge(
            &mut registry,
            "process_resident_memory_bytes",
            "Resident memory used by KeepPeek",
            value,
        );
    }
    register_gauge(
        &mut registry,
        "system_memory_total_bytes",
        "Total host memory",
        health.system.memory.total_bytes,
    );
    register_gauge(
        &mut registry,
        "system_memory_used_bytes",
        "Used host memory",
        health.system.memory.used_bytes,
    );
    register_gauge(
        &mut registry,
        "system_memory_available_bytes",
        "Available host memory",
        health.system.memory.available_bytes,
    );
    register_float_gauge(
        &mut registry,
        "system_load_1",
        "Host one-minute load average",
        health.system.load.one_minute,
    );
    register_float_gauge(
        &mut registry,
        "system_load_5",
        "Host five-minute load average",
        health.system.load.five_minutes,
    );
    register_float_gauge(
        &mut registry,
        "system_load_15",
        "Host fifteen-minute load average",
        health.system.load.fifteen_minutes,
    );

    register_gauge(
        &mut registry,
        "storage_write_buffer_bytes",
        "Configured recording write buffer capacity",
        health.storage.write_buffer_bytes,
    );
    register_gauge(
        &mut registry,
        "storage_long_term_limit_bytes",
        "Configured long-term recording storage limit, or zero when unlimited",
        health.storage.long_term_max_bytes,
    );
    register_gauge(
        &mut registry,
        "storage_minimum_free_bytes",
        "Configured minimum free recording filesystem capacity",
        health.storage.minimum_free_bytes,
    );
    register_gauge(
        &mut registry,
        "storage_maximum_used_percent",
        "Configured maximum recording filesystem usage percentage, or zero when disabled",
        u64::from(health.storage.maximum_used_percent.unwrap_or(0)),
    );
    register_gauge(
        &mut registry,
        "storage_warning_free_bytes",
        "Configured free-space cleanup warning boundary",
        health.storage.warning_free_bytes,
    );
    register_gauge(
        &mut registry,
        "storage_critical_free_bytes",
        "Configured critical free-space boundary",
        health.storage.critical_free_bytes,
    );
    register_gauge(
        &mut registry,
        "storage_cleanup_hysteresis_bytes",
        "Configured cleanup recovery headroom beyond the warning boundary",
        health.storage.cleanup_hysteresis_bytes,
    );
    register_gauge(
        &mut registry,
        "storage_pressure_state",
        "Storage pressure state: 0 normal, 1 warning, 2 critical",
        health.storage.safety.pressure.metric_value(),
    );
    register_gauge(
        &mut registry,
        "storage_recording_paused",
        "Whether recording is paused by the storage safety policy",
        u64::from(health.storage.safety.recording_state.as_str() == "paused"),
    );
    for (name, help, value) in [
        (
            "storage_effective_limit_bytes",
            "Effective KeepPeek recording limit after combining configured filesystem policies",
            health.storage.safety.effective_limit_bytes,
        ),
        (
            "storage_cleanup_target_bytes",
            "KeepPeek recording bytes targeted by the active cleanup recovery policy",
            health.storage.safety.cleanup_target_bytes,
        ),
        (
            "storage_keeppeek_bytes",
            "Catalog-owned finalized recording bytes",
            health.storage.safety.keeppeek_bytes,
        ),
        (
            "storage_filesystem_total_bytes",
            "Total capacity of the recording filesystem observed by the safety worker",
            health.storage.safety.total_bytes,
        ),
        (
            "storage_filesystem_available_bytes",
            "Available capacity of the recording filesystem observed by the safety worker",
            health.storage.safety.available_bytes,
        ),
        (
            "storage_last_cleanup_started_at_ms",
            "Unix timestamp of the most recent storage cleanup start",
            health.storage.safety.last_cleanup_started_at_ms,
        ),
        (
            "storage_last_cleanup_ended_at_ms",
            "Unix timestamp of the most recent storage cleanup end",
            health.storage.safety.last_cleanup_ended_at_ms,
        ),
        (
            "storage_last_failure_at_ms",
            "Unix timestamp of the most recent storage cleanup failure",
            health.storage.safety.last_failure_at_ms,
        ),
    ] {
        if let Some(value) = value {
            register_gauge(&mut registry, name, help, value);
        }
    }
    register_gauge(
        &mut registry,
        "storage_last_cleanup_files_removed",
        "Files removed by the most recent storage cleanup",
        health.storage.safety.last_cleanup_files_removed,
    );
    register_gauge(
        &mut registry,
        "storage_last_cleanup_bytes_removed",
        "Bytes removed by the most recent storage cleanup",
        health.storage.safety.last_cleanup_bytes_removed,
    );
    if let Some(value) = health.storage.catalog_bytes {
        register_gauge(
            &mut registry,
            "storage_catalog_bytes",
            "Recording catalog file size",
            value,
        );
    }
    register_gauge(
        &mut registry,
        "recording_demand_active_streams",
        "Recording streams with viewer or lease demand",
        health.storage.demand.active_streams,
    );
    register_gauge(
        &mut registry,
        "recording_demand_viewers",
        "Active recording stream viewer guards",
        health.storage.demand.total_viewers,
    );
    register_gauge(
        &mut registry,
        "recording_demand_leased_streams",
        "Recording streams held by review leases",
        health.storage.demand.leased_streams,
    );

    let disk_total = Family::<DiskLabels, Gauge>::default();
    let disk_available = Family::<DiskLabels, Gauge>::default();
    let disk_used = Family::<DiskLabels, Gauge>::default();
    for disk in health
        .system
        .disks
        .iter()
        .filter(|disk| disk.stores_recordings)
    {
        let labels = DiskLabels {
            name: disk.name.clone(),
            mount_point: disk.mount_point.clone(),
        };
        disk_total
            .get_or_create(&labels)
            .set(saturating_i64(disk.total_bytes));
        disk_available
            .get_or_create(&labels)
            .set(saturating_i64(disk.available_bytes));
        disk_used
            .get_or_create(&labels)
            .set(saturating_i64(disk.used_bytes));
    }
    registry.register(
        "recording_disk_total_bytes",
        "Total capacity of disks storing recordings",
        disk_total,
    );
    registry.register(
        "recording_disk_available_bytes",
        "Available capacity of disks storing recordings",
        disk_available,
    );
    registry.register(
        "recording_disk_used_bytes",
        "Used capacity of disks storing recordings",
        disk_used,
    );

    register_camera_metrics(&mut registry, health);
    register_recording_metrics(&mut registry, recording);
    register_webrtc_metrics(&mut registry, health);

    let issue_count = Family::<SeverityLabels, Gauge>::default();
    for severity in ["critical", "warning", "info"] {
        let count = health
            .issues
            .iter()
            .filter(|issue| issue.severity == severity)
            .count();
        issue_count
            .get_or_create(&SeverityLabels {
                severity: severity.to_owned(),
            })
            .set(saturating_i64(count));
    }
    registry.register(
        "health_issues",
        "Current health issue count by severity",
        issue_count,
    );

    let operational_events = Family::<OperationalEventLabels, Gauge>::default();
    for event in &health.operational_events {
        operational_events
            .get_or_create(&OperationalEventLabels {
                camera_id: event.key.camera_id.clone(),
                stream: event
                    .key
                    .stream_id
                    .clone()
                    .unwrap_or_else(|| "camera".to_owned()),
                kind: event.key.kind.as_str().to_owned(),
                severity: event.severity.as_str().to_owned(),
            })
            .set(1);
    }
    registry.register(
        "operational_event_active",
        "Active durable operational intervals by camera, stream, kind, and severity",
        operational_events,
    );

    if let Some(access) = access {
        register_counter(
            &mut registry,
            "access_authentication_successes",
            "Successful remote bearer authentications",
            access.authentication_successes,
        );
        register_counter(
            &mut registry,
            "access_authentication_failures",
            "Failed remote bearer authentications",
            access.authentication_failures,
        );
        register_counter(
            &mut registry,
            "access_authorization_denials",
            "Denied HTTP and WebRTC operations",
            access.authorization_denials,
        );
        register_counter(
            &mut registry,
            "access_sessions_created",
            "Created authenticated WebRTC sessions",
            access.sessions_created,
        );
        register_counter(
            &mut registry,
            "access_sessions_revoked_or_expired",
            "Revoked or expired authenticated WebRTC sessions",
            access.sessions_revoked_or_expired,
        );
        register_gauge(
            &mut registry,
            "access_sessions_active",
            "Currently active authenticated WebRTC sessions",
            access.active_sessions,
        );
        register_gauge(
            &mut registry,
            "access_credentials_active",
            "Configured credentials that are currently usable",
            access.active_credentials,
        );
    }

    if let Some(external_analysis) = external_analysis {
        register_external_analysis_metrics(&mut registry, external_analysis);
    }

    if let Some(backup) = backup {
        register_counter(
            &mut registry,
            "backup_operations_successes",
            "Successful authenticated backup and restore HTTP operations",
            backup.operation_successes,
        );
        register_counter(
            &mut registry,
            "backup_operations_failures",
            "Failed authenticated backup and restore HTTP operations",
            backup.operation_failures,
        );
        register_gauge(
            &mut registry,
            "backup_artifacts_retained",
            "Validated backup artifacts retained by this server",
            backup.retained_backups,
        );
        register_gauge(
            &mut registry,
            "backup_artifacts_retained_bytes",
            "Bytes used by validated retained backup artifacts",
            backup.retained_archive_bytes,
        );
        register_gauge(
            &mut registry,
            "backup_restore_active",
            "Whether a staged restore or retained rollback point is active",
            backup.active_restore,
        );
    }

    if let Some(notifications) = notifications {
        register_gauge(
            &mut registry,
            "notification_rules_configured",
            "Notification rules loaded from configuration",
            notifications.configured_rules,
        );
        register_gauge(
            &mut registry,
            "notification_deliveries_pending",
            "Notification deliveries waiting in process memory",
            notifications.pending_deliveries,
        );
        for (name, help, value) in [
            (
                "notification_candidates_accepted",
                "Notification candidates accepted by the in-memory evaluation queue",
                notifications.candidates_accepted,
            ),
            (
                "notification_candidates_dropped",
                "Notification candidates dropped because the in-memory queue was unavailable",
                notifications.candidates_dropped,
            ),
            (
                "notifications_created",
                "Logical notifications created in process memory",
                notifications.notifications_created,
            ),
            (
                "notifications_replaced",
                "Logical notifications replaced by a later event revision",
                notifications.notifications_replaced,
            ),
            (
                "notifications_suppressed",
                "Notification candidates suppressed or collapsed by policy",
                notifications.notifications_suppressed,
            ),
            (
                "notification_delivery_attempts",
                "Notification provider delivery attempts",
                notifications.delivery_attempts,
            ),
            (
                "notification_delivery_retries",
                "Notification provider retries scheduled in process memory",
                notifications.delivery_retries,
            ),
            (
                "notification_delivery_successes",
                "Successful notification provider deliveries",
                notifications.delivery_successes,
            ),
            (
                "notification_delivery_failures",
                "Permanently failed or expired notification provider deliveries",
                notifications.delivery_failures,
            ),
        ] {
            register_counter(&mut registry, name, help, value);
        }
    }

    if let Some(mqtt) = mqtt {
        register_gauge(
            &mut registry,
            "mqtt_forwarder_enabled",
            "Whether MQTT 5 event forwarding is enabled",
            u64::from(mqtt.enabled),
        );
        register_gauge(
            &mut registry,
            "mqtt_forwarder_connected",
            "Whether the MQTT 5 broker connection is healthy",
            u64::from(mqtt.state == MqttConnectionState::Connected),
        );
        register_gauge(
            &mut registry,
            "mqtt_forwarder_outbox_items",
            "In-memory MQTT publications awaiting broker acknowledgement",
            mqtt.pending_items,
        );
        register_gauge(
            &mut registry,
            "mqtt_forwarder_outbox_bytes",
            "Bytes held by in-memory MQTT publications awaiting acknowledgement",
            mqtt.pending_bytes,
        );
        register_gauge(
            &mut registry,
            "mqtt_forwarder_outbox_limit_bytes",
            "Configured MQTT in-memory outbox byte limit",
            mqtt.outbox_limit_bytes,
        );
        register_counter(
            &mut registry,
            "mqtt_forwarder_retries",
            "MQTT connection and publication retries",
            mqtt.retry_count,
        );
        register_counter(
            &mut registry,
            "mqtt_forwarder_duplicates",
            "Event revisions already known to the MQTT outbox or receipt ledger",
            mqtt.duplicate_count,
        );
        register_optional_timestamp(
            &mut registry,
            "mqtt_forwarder_last_received_timestamp_seconds",
            "Unix timestamp of the last event revision accepted by the MQTT outbox",
            mqtt.last_received_at_ms,
        );
        register_optional_timestamp(
            &mut registry,
            "mqtt_forwarder_last_delivered_timestamp_seconds",
            "Unix timestamp of the last broker-acknowledged MQTT publication",
            mqtt.last_delivered_at_ms,
        );
        register_optional_timestamp(
            &mut registry,
            "mqtt_forwarder_oldest_unacknowledged_timestamp_seconds",
            "Unix event timestamp of the oldest unacknowledged MQTT publication",
            mqtt.oldest_unacknowledged_timestamp_ms,
        );
    }

    let mut output = String::new();
    encode_registry(&mut output, &registry)?;
    Ok(output)
}

fn register_external_analysis_metrics(
    registry: &mut Registry,
    snapshot: ExternalAnalysisMetricsSnapshot,
) {
    register_external_analysis_gauges(registry, snapshot);
    register_external_analysis_subscription_counters(registry, snapshot);
    register_external_analysis_publication_counters(registry, snapshot);
    register_external_analysis_latency(registry, snapshot);
}

fn register_external_analysis_gauges(
    registry: &mut Registry,
    snapshot: ExternalAnalysisMetricsSnapshot,
) {
    for (name, help, value) in [
        (
            "external_analysis_sessions_active",
            "Current API sessions available to external analysis clients",
            snapshot.sessions_active,
        ),
        (
            "external_analysis_media_subscriptions_active",
            "Current external analysis media subscriptions",
            snapshot.media_subscriptions_active,
        ),
        (
            "external_analysis_event_subscriptions_active",
            "Current external analysis event subscriptions",
            snapshot.event_subscriptions_active,
        ),
        (
            "external_analysis_event_delivery_queue_depth",
            "Current event deliveries queued across external analysis sessions",
            snapshot.event_delivery_queue_depth,
        ),
        (
            "external_analysis_event_delivery_queue_depth_high_water",
            "Lifetime maximum external analysis event delivery queue depth",
            snapshot.event_delivery_queue_depth_high_water,
        ),
        (
            "external_analysis_event_delivery_pending_bytes",
            "Current bytes reserved for external analysis event delivery",
            snapshot.event_delivery_pending_bytes,
        ),
        (
            "external_analysis_event_delivery_pending_bytes_high_water",
            "Lifetime maximum bytes reserved for external analysis event delivery",
            snapshot.event_delivery_pending_bytes_high_water,
        ),
        (
            "external_analysis_event_publications_active",
            "Current external analysis event publications accepting, waiting, or committing",
            snapshot.event_publications_active,
        ),
        (
            "external_analysis_event_publication_staged_bytes",
            "Current attachment bytes staged by external analysis event publications",
            snapshot.event_publication_staged_bytes,
        ),
    ] {
        register_gauge(registry, name, help, value);
    }
}

fn register_external_analysis_subscription_counters(
    registry: &mut Registry,
    snapshot: ExternalAnalysisMetricsSnapshot,
) {
    for (name, help, value) in [
        (
            "external_analysis_event_subscription_starts",
            "External analysis event subscriptions admitted",
            snapshot.event_subscription_starts,
        ),
        (
            "external_analysis_event_subscription_rejections",
            "External analysis event subscriptions rejected",
            snapshot.event_subscription_rejections,
        ),
        (
            "external_analysis_event_subscription_deliveries",
            "Committed events matched to external analysis subscriptions",
            snapshot.event_subscription_deliveries,
        ),
        (
            "external_analysis_event_subscription_sheds",
            "External analysis event subscriptions shed after queue refusal",
            snapshot.event_subscription_sheds,
        ),
        (
            "external_analysis_event_deliveries_queued",
            "External analysis event deliveries accepted by API session queues",
            snapshot.event_deliveries_queued,
        ),
        (
            "external_analysis_event_delivery_drops",
            "External analysis event deliveries rejected by API session queues",
            snapshot.event_delivery_drops,
        ),
    ] {
        register_counter(registry, name, help, value);
    }
}

fn register_external_analysis_publication_counters(
    registry: &mut Registry,
    snapshot: ExternalAnalysisMetricsSnapshot,
) {
    for (name, help, value) in [
        (
            "external_analysis_event_publication_starts",
            "External analysis event publications started",
            snapshot.event_publication_starts,
        ),
        (
            "external_analysis_event_publication_commits",
            "External analysis event publications committed durably",
            snapshot.event_publication_commits,
        ),
        (
            "external_analysis_event_publication_aborts",
            "External analysis event publications aborted",
            snapshot.event_publication_aborts,
        ),
        (
            "external_analysis_event_publication_expirations",
            "External analysis event publications expired",
            snapshot.event_publication_expirations,
        ),
        (
            "external_analysis_event_publication_rejections",
            "External analysis publication commands or chunks rejected",
            snapshot.event_publication_rejections,
        ),
        (
            "external_analysis_event_publication_storage_failures",
            "External analysis publication operations rejected by unavailable storage",
            snapshot.event_publication_storage_failures,
        ),
    ] {
        register_counter(registry, name, help, value);
    }
}

fn register_external_analysis_latency(
    registry: &mut Registry,
    snapshot: ExternalAnalysisMetricsSnapshot,
) {
    let commit_latency = Family::<QuantileLabels, Gauge>::default();
    for (quantile, value) in [
        ("p50", snapshot.event_publication_commit_latency_ms_p50),
        ("p95", snapshot.event_publication_commit_latency_ms_p95),
    ] {
        commit_latency
            .get_or_create(&QuantileLabels {
                quantile: quantile.to_owned(),
            })
            .set(saturating_i64(value));
    }
    registry.register(
        "external_analysis_event_publication_commit_latency_milliseconds",
        "Recent external analysis publication latency from start through durable commit",
        commit_latency,
    );
}

fn register_optional_timestamp(
    registry: &mut Registry,
    name: &'static str,
    help: &'static str,
    timestamp_ms: Option<i64>,
) {
    let gauge = FloatGauge::default();
    gauge.set(timestamp_ms.map_or(0.0, |value| value as f64 / 1_000.0));
    registry.register(name, help, gauge);
}

fn register_recording_metrics(
    registry: &mut Registry,
    snapshot: Option<&RecordingCoverageMetricSnapshot>,
) {
    register_gauge(
        registry,
        "recording_coverage_snapshot_available",
        "Whether the canonical recording coverage snapshot was available",
        u64::from(snapshot.is_some_and(|snapshot| snapshot.catalog_available)),
    );
    let Some(snapshot) = snapshot else {
        return;
    };
    register_gauge(
        registry,
        "recording_catalog_revision",
        "Catalog revision used by recording coverage metrics",
        snapshot.catalog_revision,
    );
    register_gauge(
        registry,
        "recording_coverage_window_start_seconds",
        "Unix timestamp at the start of the selected recording coverage window",
        snapshot.window.start_ms.div_euclid(1_000),
    );
    register_gauge(
        registry,
        "recording_coverage_window_end_seconds",
        "Unix timestamp at the end of the selected recording coverage window",
        snapshot.window.end_ms.div_euclid(1_000),
    );

    let requested = Family::<CameraStreamLabels, Gauge>::default();
    let writer = Family::<RecordingWriterLabels, Gauge>::default();
    let last_frame = Family::<CameraStreamLabels, Gauge>::default();
    let last_write = Family::<CameraStreamLabels, Gauge>::default();
    let last_finalize = Family::<CameraStreamLabels, Gauge>::default();
    let last_catalog_commit = Family::<CameraStreamLabels, Gauge>::default();
    let oldest_retained = Family::<CameraStreamLabels, Gauge>::default();
    let newest_retained = Family::<CameraStreamLabels, Gauge>::default();
    let effective_retention = Family::<CameraStreamLabels, Gauge>::default();
    let recording_bytes = Family::<CameraStreamLabels, Gauge>::default();
    let estimated_bytes_per_day = Family::<CameraStreamLabels, Gauge>::default();
    let selected_coverage = Family::<CameraStreamLabels, Gauge>::default();
    let coverage_ratio = Family::<CameraStreamLabels, FloatGauge>::default();
    let gap_count = Family::<CameraStreamLabels, Gauge>::default();
    let largest_gap = Family::<CameraStreamLabels, Gauge>::default();
    let current_gap = Family::<CameraStreamLabels, Gauge>::default();

    for camera in &snapshot.cameras {
        for stream in &camera.streams {
            let labels = CameraStreamLabels {
                camera_id: camera.camera_id.clone(),
                camera_name: camera.camera_name.clone(),
                stream: stream.stream_id.clone(),
            };
            requested
                .get_or_create(&labels)
                .set(i64::from(stream.recording_requested));
            writer
                .get_or_create(&RecordingWriterLabels {
                    camera_id: camera.camera_id.clone(),
                    camera_name: camera.camera_name.clone(),
                    stream: stream.stream_id.clone(),
                    state: stream.writer_state.as_str().to_owned(),
                })
                .set(1);
            if let Some(value) = stream.last_frame_at_ms {
                last_frame
                    .get_or_create(&labels)
                    .set(saturating_i64(value / 1_000));
            }
            if let Some(value) = stream.last_write_at_ms {
                last_write
                    .get_or_create(&labels)
                    .set(saturating_i64(value / 1_000));
            }
            if let Some(value) = stream.last_finalize_at_ms {
                last_finalize
                    .get_or_create(&labels)
                    .set(value.div_euclid(1_000));
            }
            if let Some(value) = stream.last_catalog_commit_at_ms {
                last_catalog_commit
                    .get_or_create(&labels)
                    .set(value.div_euclid(1_000));
            }
            if let Some(value) = stream.oldest_retained_at_ms {
                oldest_retained
                    .get_or_create(&labels)
                    .set(value.div_euclid(1_000));
            }
            if let Some(value) = stream.newest_retained_at_ms {
                newest_retained
                    .get_or_create(&labels)
                    .set(value.div_euclid(1_000));
            }
            effective_retention
                .get_or_create(&labels)
                .set(saturating_i64(
                    stream.effective_retention_ms.unwrap_or(0) / 1_000,
                ));
            recording_bytes
                .get_or_create(&labels)
                .set(saturating_i64(stream.recording_bytes));
            estimated_bytes_per_day
                .get_or_create(&labels)
                .set(saturating_i64(stream.estimated_bytes_per_day));
            selected_coverage
                .get_or_create(&labels)
                .set(saturating_i64(stream.selected_coverage_ms / 1_000));
            coverage_ratio
                .get_or_create(&labels)
                .set(stream.coverage_percent / 100.0);
            gap_count
                .get_or_create(&labels)
                .set(saturating_i64(stream.gap_count));
            largest_gap
                .get_or_create(&labels)
                .set(saturating_i64(stream.largest_gap_ms / 1_000));
            current_gap.get_or_create(&labels).set(saturating_i64(
                stream
                    .gaps
                    .iter()
                    .find(|gap| gap.end_ms.is_none())
                    .map_or(0, |gap| gap.duration_ms / 1_000),
            ));
        }
    }

    for (name, help, family) in [
        (
            "recording_policy_requested",
            "Whether the effective camera policy requests this recording stream",
            requested,
        ),
        (
            "recording_last_frame_timestamp_seconds",
            "Unix timestamp of the most recent frame observed for this stream",
            last_frame,
        ),
        (
            "recording_last_write_timestamp_seconds",
            "Unix timestamp of the most recent successful writer progress",
            last_write,
        ),
        (
            "recording_last_finalize_timestamp_seconds",
            "Unix timestamp of the most recent finalized recording file",
            last_finalize,
        ),
        (
            "recording_last_catalog_commit_timestamp_seconds",
            "Unix timestamp of the catalog revision used by this snapshot",
            last_catalog_commit,
        ),
        (
            "recording_oldest_retained_timestamp_seconds",
            "Unix timestamp of the oldest retained playable fragment",
            oldest_retained,
        ),
        (
            "recording_newest_retained_timestamp_seconds",
            "Unix timestamp at the end of the newest retained playable fragment",
            newest_retained,
        ),
        (
            "recording_effective_retention_seconds",
            "Elapsed span between oldest and newest retained playable media",
            effective_retention,
        ),
        (
            "recording_storage_bytes",
            "Finalized recording file bytes attributed to this stream",
            recording_bytes,
        ),
        (
            "recording_estimated_bytes_per_day",
            "Selected playable fragment bytes scaled to one day",
            estimated_bytes_per_day,
        ),
        (
            "recording_selected_coverage_seconds",
            "Merged playable duration in the selected coverage window",
            selected_coverage,
        ),
        (
            "recording_gap_count",
            "Gap count in the selected coverage window",
            gap_count,
        ),
        (
            "recording_largest_gap_seconds",
            "Largest gap duration in the selected coverage window",
            largest_gap,
        ),
        (
            "recording_current_gap_seconds",
            "Observed duration of the current open recording gap",
            current_gap,
        ),
    ] {
        registry.register(name, help, family);
    }
    registry.register(
        "recording_writer_state",
        "Current writer state by camera and stream",
        writer,
    );
    registry.register(
        "recording_coverage_ratio",
        "Playable fraction of the selected recording coverage window",
        coverage_ratio,
    );
}

fn register_camera_metrics(registry: &mut Registry, health: &ServerHealthResponse) {
    let camera_info = Family::<CameraStateLabels, Gauge>::default();
    let camera_dimensions = Family::<CameraDimensionLabels, Gauge>::default();
    let camera_dimensions_known = Family::<CameraDimensionLabels, Gauge>::default();
    let stream_fps = Family::<CameraStreamLabels, FloatGauge>::default();
    let stream_bitrate = Family::<CameraStreamLabels, FloatGauge>::default();
    let stream_frames = Family::<CameraStreamLabels, Counter>::default();
    let stream_bytes = Family::<CameraStreamLabels, Counter>::default();
    let stream_drops = Family::<CameraStreamLabels, Counter>::default();
    let stream_errors = Family::<CameraStreamLabels, Counter>::default();
    let stream_reconnects = Family::<CameraStreamLabels, Counter>::default();
    let stream_dimensions = Family::<CameraStreamDimensionLabels, Gauge>::default();
    let stream_dimensions_known = Family::<CameraStreamDimensionLabels, Gauge>::default();

    for camera in &health.cameras {
        camera_info
            .get_or_create(&CameraStateLabels {
                camera_id: camera.id.clone(),
                camera_name: camera.name.clone(),
                state: camera.state.as_str().to_owned(),
            })
            .set(1);
        for (dimension, value) in [
            ("transport_connected", camera.dimensions.transport_connected),
            ("frames_fresh", camera.dimensions.frames_fresh),
            ("decodable", camera.dimensions.decodable),
            (
                "recording_requested",
                Some(camera.dimensions.recording_requested),
            ),
            (
                "recording_progressing",
                camera.dimensions.recording_progressing,
            ),
            ("battery_sleeping", camera.dimensions.battery_sleeping),
        ] {
            let labels = CameraDimensionLabels {
                camera_id: camera.id.clone(),
                camera_name: camera.name.clone(),
                dimension: dimension.to_owned(),
            };
            camera_dimensions
                .get_or_create(&labels)
                .set(i64::from(value == Some(true)));
            camera_dimensions_known
                .get_or_create(&labels)
                .set(i64::from(value.is_some()));
        }

        for stream in &camera.streams {
            let report = &stream.ingress.report;
            let labels = CameraStreamLabels {
                camera_id: camera.id.clone(),
                camera_name: camera.name.clone(),
                stream: report.kind.clone(),
            };
            stream_fps.get_or_create(&labels).set(report.fps);
            stream_bitrate
                .get_or_create(&labels)
                .set(report.kbps * 1_000.0);
            stream_frames
                .get_or_create(&labels)
                .inc_by(report.frames.unwrap_or(0));
            stream_bytes
                .get_or_create(&labels)
                .inc_by(report.bytes.unwrap_or(0));
            stream_drops
                .get_or_create(&labels)
                .inc_by(report.drops.unwrap_or(0));
            stream_errors
                .get_or_create(&labels)
                .inc_by(report.errors.unwrap_or(0));
            stream_reconnects
                .get_or_create(&labels)
                .inc_by(report.reconnects.unwrap_or(0));
            for (dimension, value) in [
                ("transport_connected", stream.dimensions.transport_connected),
                ("report_fresh", Some(stream.dimensions.report_fresh)),
                ("frames_fresh", Some(stream.dimensions.frames_fresh)),
                ("decodable", Some(stream.dimensions.decodable)),
                (
                    "recording_requested",
                    Some(stream.dimensions.recording_requested),
                ),
                (
                    "recording_progressing",
                    stream.dimensions.recording_progressing,
                ),
            ] {
                let labels = CameraStreamDimensionLabels {
                    camera_id: camera.id.clone(),
                    camera_name: camera.name.clone(),
                    stream: report.kind.clone(),
                    dimension: dimension.to_owned(),
                };
                stream_dimensions
                    .get_or_create(&labels)
                    .set(i64::from(value == Some(true)));
                stream_dimensions_known
                    .get_or_create(&labels)
                    .set(i64::from(value.is_some()));
            }
        }
    }

    registry.register(
        "camera_info",
        "Camera identity and current runtime state",
        camera_info,
    );
    registry.register(
        "camera_health_dimension",
        "Current camera health dimension value; consult camera_health_dimension_known",
        camera_dimensions,
    );
    registry.register(
        "camera_health_dimension_known",
        "Whether the current camera health dimension has authoritative evidence",
        camera_dimensions_known,
    );
    registry.register(
        "camera_ingress_frames_per_second",
        "Current camera stream ingress frame rate",
        stream_fps,
    );
    registry.register(
        "camera_ingress_bitrate_bits_per_second",
        "Current camera stream ingress bitrate",
        stream_bitrate,
    );
    registry.register(
        "camera_ingress_frames",
        "Cumulative camera stream ingress frames",
        stream_frames,
    );
    registry.register(
        "camera_ingress_bytes",
        "Cumulative camera stream ingress bytes",
        stream_bytes,
    );
    registry.register(
        "camera_ingress_drops",
        "Cumulative camera stream ingress frame drops",
        stream_drops,
    );
    registry.register(
        "camera_ingress_errors",
        "Cumulative camera stream ingress errors",
        stream_errors,
    );
    registry.register(
        "camera_ingress_reconnects",
        "Cumulative camera stream reconnects",
        stream_reconnects,
    );
    registry.register(
        "camera_stream_health_dimension",
        "Current stream health dimension value; consult camera_stream_health_dimension_known",
        stream_dimensions,
    );
    registry.register(
        "camera_stream_health_dimension_known",
        "Whether the current stream health dimension has authoritative evidence",
        stream_dimensions_known,
    );
}

fn register_webrtc_metrics(registry: &mut Registry, health: &ServerHealthResponse) {
    let webrtc = &health.webrtc;
    register_gauge(
        registry,
        "webrtc_active_sessions",
        "Current WebRTC sessions",
        webrtc.active_sessions,
    );
    register_gauge(
        registry,
        "webrtc_multi_track_sessions",
        "Current API WebRTC client sessions",
        webrtc.multi_track_sessions,
    );
    register_gauge(
        registry,
        "webrtc_multi_tracks",
        "Current media tracks across API WebRTC client sessions",
        webrtc.multi_tracks,
    );
    register_gauge(
        registry,
        "webrtc_active_main_streams",
        "Current main-stream WebRTC subscriptions",
        webrtc.active_main,
    );
    register_gauge(
        registry,
        "webrtc_active_sub_streams",
        "Current sub-stream WebRTC subscriptions",
        webrtc.active_sub,
    );
    register_counter(
        registry,
        "webrtc_published_frames",
        "Cumulative frames published to WebRTC",
        webrtc.published_frames,
    );
    register_counter(
        registry,
        "webrtc_published_bytes",
        "Cumulative bytes published to WebRTC",
        webrtc.published_bytes,
    );
    register_counter(
        registry,
        "webrtc_delivered_frames",
        "Cumulative frames queued for WebRTC delivery",
        webrtc.delivered_frames,
    );
    register_counter(
        registry,
        "webrtc_written_frames",
        "Cumulative frames written to WebRTC transports",
        webrtc.written_frames,
    );
    register_gauge(
        registry,
        "webrtc_queued_frames",
        "Current frames queued across WebRTC subscriptions",
        webrtc.queued_frames,
    );
    register_gauge(
        registry,
        "webrtc_queue_depth_max",
        "Current maximum WebRTC subscription queue depth",
        webrtc.queue_depth_max,
    );
    register_gauge(
        registry,
        "webrtc_queue_high_water",
        "Lifetime maximum WebRTC subscription queue depth",
        webrtc.queue_high_water,
    );
    register_counter(
        registry,
        "webrtc_queue_drops",
        "Cumulative frames dropped because WebRTC queues were full",
        webrtc.queue_drops,
    );
    register_counter(
        registry,
        "webrtc_queue_discarded_frames",
        "Cumulative queued WebRTC frames discarded",
        webrtc.queue_discarded_frames,
    );
    register_counter(
        registry,
        "webrtc_queue_recovery_drops",
        "Cumulative WebRTC frames dropped during keyframe recovery",
        webrtc.queue_recovery_drops,
    );

    let source_subscribers = Family::<WebRtcSourceLabels, Gauge>::default();
    let source_bitrate = Family::<WebRtcSourceLabels, Gauge>::default();
    let source_has_keyframe = Family::<WebRtcSourceLabels, Gauge>::default();
    for source in &webrtc.sources {
        let labels = WebRtcSourceLabels {
            camera_ip: source.camera_ip.to_string(),
            stream: source.stream.to_string(),
        };
        source_subscribers
            .get_or_create(&labels)
            .set(saturating_i64(source.subscribers));
        source_bitrate
            .get_or_create(&labels)
            .set(saturating_i64(source.bitrate_bps.unwrap_or(0)));
        source_has_keyframe
            .get_or_create(&labels)
            .set(i64::from(source.has_keyframe));
    }
    registry.register(
        "webrtc_source_subscribers",
        "Current WebRTC subscribers by camera source",
        source_subscribers,
    );
    registry.register(
        "webrtc_source_bitrate_bits_per_second",
        "Estimated camera source bitrate available to WebRTC",
        source_bitrate,
    );
    registry.register(
        "webrtc_source_has_keyframe",
        "Whether WebRTC has a cached keyframe for the camera source",
        source_has_keyframe,
    );
}

fn register_gauge(
    registry: &mut Registry,
    name: &'static str,
    help: &'static str,
    value: impl TryInto<i64>,
) {
    let gauge: Gauge = Gauge::default();
    gauge.set(value.try_into().unwrap_or(i64::MAX));
    registry.register(name, help, gauge);
}

fn register_float_gauge(
    registry: &mut Registry,
    name: &'static str,
    help: &'static str,
    value: f64,
) {
    let gauge = FloatGauge::default();
    gauge.set(value);
    registry.register(name, help, gauge);
}

fn register_counter(registry: &mut Registry, name: &'static str, help: &'static str, value: u64) {
    let counter: Counter = Counter::default();
    counter.inc_by(value);
    registry.register(name, help, counter);
}

fn saturating_i64(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}
