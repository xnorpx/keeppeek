use super::{CameraEntry, ControlCommandError, ServerState, millis_timestamp, server_health};
use crate::{
    api::{
        ProfileSummary,
        proto::{self, health_command, ok as control_ok},
    },
    health::{
        CameraHealth, CameraHealthDimensions, HealthIssue, HealthTotals, ServerHealthResponse,
        StorageHealth, StreamHealth,
    },
    operational_events::OperationalEvent,
    runtime::{FacadeSender, RouterMessage},
    stats::StreamHealthReport,
};
use std::collections::BTreeMap;

pub(super) fn dispatch(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    command: proto::HealthCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    match command.action {
        Some(health_command::Action::Get(_)) => Ok(control_ok::Result::HealthResult(
            proto_health_snapshot(server_health(router_tx, state)),
        )),
        None => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "health command has no action",
        )),
    }
}

pub(super) fn proto_health_snapshot(health: ServerHealthResponse) -> proto::ServerHealthSnapshot {
    proto::ServerHealthSnapshot {
        status: health.status,
        generated_at_ms: health.generated_at_ms,
        uptime_seconds: health.uptime_seconds,
        version: health.version.to_owned(),
        totals: Some(proto_health_totals(health.totals)),
        system: Some(proto_system_health(health.system)),
        storage: Some(proto_storage_health(health.storage)),
        webrtc: Some(proto_webrtc_health(health.webrtc)),
        cameras: health
            .cameras
            .into_iter()
            .map(proto_camera_health)
            .collect(),
        issues: health
            .issues
            .into_iter()
            .map(|issue| proto::HealthIssueSnapshot {
                severity: issue.severity,
                scope: issue.scope,
                message: issue.message,
                operational_event_id: issue.operational_event_id,
                timeline_start: issue.timeline_start_ms.map(millis_timestamp),
                timeline_end: issue.timeline_end_ms.map(millis_timestamp),
            })
            .collect(),
        health_contract_version: health.health_contract_version,
        operational_events: health
            .operational_events
            .into_iter()
            .map(proto_operational_event)
            .collect(),
    }
}

pub(super) fn proto_operational_event(event: OperationalEvent) -> proto::Event {
    let mut fields = BTreeMap::new();
    fields.insert(
        "severity".to_owned(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::StringValue(
                event.severity.as_str().to_owned(),
            )),
        },
    );
    fields.insert(
        "cause".to_owned(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::StringValue(
                event.evidence.cause.clone(),
            )),
        },
    );
    fields.insert(
        "affected_streams".to_owned(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::ListValue(
                prost_types::ListValue {
                    values: event
                        .evidence
                        .affected_streams
                        .iter()
                        .map(|stream| prost_types::Value {
                            kind: Some(prost_types::value::Kind::StringValue(stream.clone())),
                        })
                        .collect(),
                },
            )),
        },
    );
    fields.insert(
        "recording_interrupted".to_owned(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::BoolValue(
                event.evidence.recording_interrupted,
            )),
        },
    );
    fields.insert(
        "evidence_source".to_owned(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::StringValue(
                event.evidence.source.clone(),
            )),
        },
    );
    fields.insert(
        "recovered".to_owned(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::BoolValue(
                event.end_time_ms.is_some(),
            )),
        },
    );
    if let Some(stream_id) = &event.key.stream_id {
        fields.insert(
            "stream_id".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue(stream_id.clone())),
            },
        );
    }
    if let Some(duration_ms) = event.duration_ms {
        fields.insert(
            "duration_ms".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::NumberValue(duration_ms as f64)),
            },
        );
    }
    proto::Event {
        event_id: event.id,
        revision: event.revision,
        source_id: event.key.camera_id,
        media_kind: event
            .key
            .stream_id
            .as_ref()
            .map(|_| proto::MediaKind::Video as i32),
        origin: proto::EventOrigin::Keeppeek as i32,
        event_type: event.key.kind.as_str().to_owned(),
        start_time: Some(millis_timestamp(event.start_time_ms)),
        end_time: event.end_time_ms.map(millis_timestamp),
        confidence: None,
        bounding_box: None,
        zone: None,
        text: Some(event.evidence.explanation),
        payload: Some(prost_types::Struct { fields }),
        attachments: Vec::new(),
        source_session_id: None,
        subscription_id: None,
        canonical_attachment_id: None,
        icon_key: Some("alert".to_owned()),
        rejected_icon_key: None,
        bounding_box_attachment_id: None,
        image_availability: proto::EventImageAvailability::None as i32,
    }
}

fn proto_health_totals(totals: HealthTotals) -> proto::HealthTotalsSnapshot {
    proto::HealthTotalsSnapshot {
        configured_cameras: usize_u64(totals.configured_cameras),
        configured_video_streams: usize_u64(totals.configured_video_streams),
        ingress_fps: totals.ingress_fps,
        ingress_bitrate_bps: totals.ingress_bitrate_bps,
        frames: totals.frames,
        keyframes: totals.keyframes,
        drops: totals.drops,
        errors: totals.errors,
        reconnects: totals.reconnects,
        connected_cameras: usize_u64(totals.connected_cameras),
        fresh_cameras: usize_u64(totals.fresh_cameras),
        decodable_cameras: usize_u64(totals.decodable_cameras),
        recording_requested_cameras: usize_u64(totals.recording_requested_cameras),
        recording_cameras: usize_u64(totals.recording_cameras),
        unknown_cameras: usize_u64(totals.unknown_cameras),
        connected_video_streams: usize_u64(totals.connected_video_streams),
        fresh_video_streams: usize_u64(totals.fresh_video_streams),
        decodable_video_streams: usize_u64(totals.decodable_video_streams),
        recording_requested_video_streams: usize_u64(totals.recording_requested_video_streams),
        recording_video_streams: usize_u64(totals.recording_video_streams),
    }
}

fn proto_camera_health(camera: CameraHealth) -> proto::CameraHealthSnapshot {
    proto::CameraHealthSnapshot {
        id: camera.id,
        ip: camera.ip,
        name: camera.name,
        manufacturer: camera.manufacturer,
        model: camera.model,
        firmware_version: camera.firmware_version,
        backend: camera.backend,
        transport: camera.transport,
        state: camera.state.as_str().to_owned(),
        lifecycle: camera.lifecycle,
        last_error: camera.last_error,
        configured_profiles: camera
            .configured_profiles
            .into_iter()
            .map(proto_health_profile)
            .collect(),
        streams: camera
            .streams
            .into_iter()
            .map(proto_stream_health)
            .collect(),
        reason: camera.reason.as_str().to_owned(),
        reason_codes: camera
            .reason_codes
            .into_iter()
            .map(|reason| reason.as_str().to_owned())
            .collect(),
        detail: camera.detail,
        dimensions: Some(proto_camera_health_dimensions(camera.dimensions)),
    }
}

fn proto_camera_health_dimensions(
    dimensions: CameraHealthDimensions,
) -> proto::CameraHealthDimensionsSnapshot {
    let connected_video_stream_ids_known = dimensions.connected_video_stream_ids.is_some();
    proto::CameraHealthDimensionsSnapshot {
        configured: dimensions.configured,
        expected: dimensions.expected,
        configured_video_streams: usize_u64(dimensions.configured_video_streams),
        connected_video_streams: dimensions.connected_video_streams.map(usize_u64),
        reporting_video_streams: usize_u64(dimensions.reporting_video_streams),
        fresh_video_streams: usize_u64(dimensions.fresh_video_streams),
        decodable_video_streams: usize_u64(dimensions.decodable_video_streams),
        configured_video_stream_ids: dimensions.configured_video_stream_ids,
        connected_video_stream_ids: dimensions.connected_video_stream_ids.unwrap_or_default(),
        connected_video_stream_ids_known,
        reporting_video_stream_ids: dimensions.reporting_video_stream_ids,
        fresh_video_stream_ids: dimensions.fresh_video_stream_ids,
        decodable_video_stream_ids: dimensions.decodable_video_stream_ids,
        transport_connected: dimensions.transport_connected,
        latest_report_at_ms: dimensions.latest_report_at_ms,
        report_age_ms: dimensions.report_age_ms,
        frames_fresh: dimensions.frames_fresh,
        decodable: dimensions.decodable,
        recent_reconnects: dimensions.recent_reconnects,
        recent_drops: dimensions.recent_drops,
        recent_errors: dimensions.recent_errors,
        recording_requested: dimensions.recording_requested,
        recording_video_streams: usize_u64(dimensions.recording_video_streams),
        recording_streams_progressing: usize_u64(dimensions.recording_streams_progressing),
        recording_video_stream_ids: dimensions.recording_video_stream_ids,
        recording_progressing_stream_ids: dimensions.recording_progressing_stream_ids,
        recording_progressing: dimensions.recording_progressing,
        recording_progress_age_ms: dimensions.recording_progress_age_ms,
        session_duration_ms: dimensions.session_duration_ms,
        recorded_main_duration_ms: dimensions.recorded_main_duration_ms,
        recorded_sub_duration_ms: dimensions.recorded_sub_duration_ms,
        recorded_total_duration_ms: dimensions.recorded_total_duration_ms,
        battery_configured: dimensions.battery_configured,
        battery_registered: dimensions.battery_registered,
        battery_last_seen_age_ms: dimensions.battery_last_seen_age_ms,
        battery_sleeping: dimensions.battery_sleeping,
        battery_wake_pending_age_ms: dimensions.battery_wake_pending_age_ms,
    }
}

pub(super) fn proto_health_profile(profile: ProfileSummary) -> proto::HealthProfileSummary {
    proto::HealthProfileSummary {
        name: profile.name,
        stream: profile.stream,
        encoding: profile.encoding,
        resolution: profile.resolution,
        framerate: profile.framerate,
        bitrate_kbps: profile.bitrate_kbps,
        gop: profile.gop,
        h264_profile: profile.h264_profile,
        audio: profile.audio.map(|audio| proto::HealthAudioProfileSummary {
            encoding: audio.encoding,
            sample_rate: audio.sample_rate,
            bitrate_kbps: audio.bitrate_kbps,
        }),
    }
}

fn proto_stream_health(stream: StreamHealth) -> proto::StreamHealthSnapshot {
    let dimensions = stream.dimensions;
    let ingress = stream.ingress;
    let report = ingress.report;
    proto::StreamHealthSnapshot {
        r#type: report.kind,
        codec: report.codec,
        resolution: report.resolution,
        fps: nonzero_f64(report.fps),
        expected_fps: nonzero_f64(report.expected_fps),
        kf_fps: nonzero_f64(report.kf_fps),
        kbps: nonzero_f64(report.kbps),
        max_frame_kb: nonzero_f64(report.max_frame_kb),
        gap_min_ms: nonzero_f64(report.gap_min_ms),
        gap_avg_ms: nonzero_f64(report.gap_avg_ms),
        gap_max_ms: nonzero_f64(report.gap_max_ms),
        jitter_samples: nonzero_u64(report.jitter_samples),
        jitter_p50_ms: nonzero_f64(report.jitter_p50_ms),
        jitter_p99_ms: nonzero_f64(report.jitter_p99_ms),
        frames: report.frames.and_then(nonzero_u64),
        bytes: report.bytes.and_then(nonzero_u64),
        keyframes: report.keyframes.and_then(nonzero_u64),
        reconnects: report.reconnects.and_then(nonzero_u64),
        drops: report.drops.and_then(nonzero_u64),
        errors: report.errors.and_then(nonzero_u64),
        updated_at_ms: ingress.updated_at_ms,
        report_age_ms: ingress.report_age_ms,
        frame_updated_at_ms: ingress.frame_updated_at_ms,
        frame_age_ms: ingress.frame_age_ms,
        keyframe_updated_at_ms: ingress.keyframe_updated_at_ms,
        keyframe_age_ms: ingress.keyframe_age_ms,
        recent_reconnects: ingress.recent_reconnects,
        recent_drops: ingress.recent_drops,
        recent_errors: ingress.recent_errors,
        state: stream.state.as_str().to_owned(),
        reason: stream.reason.as_str().to_owned(),
        reason_codes: stream
            .reason_codes
            .into_iter()
            .map(|reason| reason.as_str().to_owned())
            .collect(),
        detail: stream.detail,
        dimensions: Some(proto::StreamHealthDimensionsSnapshot {
            expected: dimensions.expected,
            transport_connected: dimensions.transport_connected,
            report_fresh: dimensions.report_fresh,
            report_freshness_threshold_ms: dimensions.report_freshness_threshold_ms,
            frames_fresh: dimensions.frames_fresh,
            frame_freshness_threshold_ms: dimensions.frame_freshness_threshold_ms,
            decodable: dimensions.decodable,
            keyframe_freshness_threshold_ms: dimensions.keyframe_freshness_threshold_ms,
            recent_reconnects: dimensions.recent_reconnects,
            recent_drops: dimensions.recent_drops,
            recent_errors: dimensions.recent_errors,
            recording_requested: dimensions.recording_requested,
            recording_progressing: dimensions.recording_progressing,
            recording_progress_age_ms: dimensions.recording_progress_age_ms,
            session_duration_ms: dimensions.session_duration_ms,
            recorded_duration_ms: dimensions.recorded_duration_ms,
        }),
    }
}

fn proto_system_health(system: crate::health::SystemHealth) -> proto::SystemHealthSnapshot {
    proto::SystemHealthSnapshot {
        host_name: system.host_name,
        os_name: system.os_name,
        os_version: system.os_version,
        kernel_version: system.kernel_version,
        architecture: system.architecture.to_owned(),
        system_uptime_seconds: system.system_uptime_seconds,
        boot_time_seconds: system.boot_time_seconds,
        logical_cores: usize_u64(system.logical_cores),
        physical_cores: system.physical_cores.map(usize_u64),
        cpu_brand: system.cpu_brand,
        system_cpu_percent: system.system_cpu_percent,
        process: Some(proto_process_health(system.process)),
        memory: Some(proto::MemoryHealthSnapshot {
            total_bytes: system.memory.total_bytes,
            used_bytes: system.memory.used_bytes,
            available_bytes: system.memory.available_bytes,
            total_swap_bytes: system.memory.total_swap_bytes,
            used_swap_bytes: system.memory.used_swap_bytes,
        }),
        load: Some(proto::LoadHealthSnapshot {
            one_minute: system.load.one_minute,
            five_minutes: system.load.five_minutes,
            fifteen_minutes: system.load.fifteen_minutes,
        }),
        cpus: system
            .cpus
            .into_iter()
            .map(|cpu| proto::CpuHealthSnapshot {
                name: cpu.name,
                usage_percent: cpu.usage_percent,
                frequency_mhz: cpu.frequency_mhz,
            })
            .collect(),
        network_egress_bps: system.network_egress_bps,
        networks: system
            .networks
            .into_iter()
            .map(|network| proto::NetworkHealthSnapshot {
                name: network.name,
                received_bytes_per_second: network.received_bytes_per_second,
                transmitted_bytes_per_second: network.transmitted_bytes_per_second,
                received_packets_per_second: network.received_packets_per_second,
                transmitted_packets_per_second: network.transmitted_packets_per_second,
                receive_errors: network.receive_errors,
                transmit_errors: network.transmit_errors,
                total_received_bytes: network.total_received_bytes,
                total_transmitted_bytes: network.total_transmitted_bytes,
            })
            .collect(),
        disks: system
            .disks
            .into_iter()
            .map(|disk| proto::DiskHealthSnapshot {
                name: disk.name,
                kind: disk.kind,
                file_system: disk.file_system,
                mount_point: disk.mount_point,
                total_bytes: disk.total_bytes,
                available_bytes: disk.available_bytes,
                used_bytes: disk.used_bytes,
                removable: disk.removable,
                stores_recordings: disk.stores_recordings,
            })
            .collect(),
        temperatures: system
            .temperatures
            .into_iter()
            .map(|temperature| proto::TemperatureHealthSnapshot {
                label: temperature.label,
                current_celsius: temperature.current_celsius,
                max_celsius: temperature.max_celsius,
                critical_celsius: temperature.critical_celsius,
            })
            .collect(),
    }
}

fn proto_process_health(process: crate::health::ProcessHealth) -> proto::ProcessHealthSnapshot {
    proto::ProcessHealthSnapshot {
        pid: process.pid,
        name: process.name,
        executable: process.executable,
        working_directory: process.working_directory,
        cpu_percent: process.cpu_percent,
        cpu_capacity_percent: process.cpu_capacity_percent,
        cpu_core_equivalents: process.cpu_core_equivalents,
        resident_memory_bytes: process.resident_memory_bytes,
        memory_capacity_percent: process.memory_capacity_percent,
        virtual_memory_bytes: process.virtual_memory_bytes,
        started_at_seconds: process.started_at_seconds,
        uptime_seconds: process.uptime_seconds,
        tasks: process.tasks.map(usize_u64),
        read_bytes_per_second: process.read_bytes_per_second,
        write_bytes_per_second: process.write_bytes_per_second,
        total_read_bytes: process.total_read_bytes,
        total_written_bytes: process.total_written_bytes,
    }
}

fn proto_storage_health(storage: StorageHealth) -> proto::StorageHealthSnapshot {
    let safety = storage.safety;
    proto::StorageHealthSnapshot {
        medium_term_path: storage.medium_term_path,
        long_term_path: storage.long_term_path,
        paths_are_same: storage.paths_are_same,
        short_term_seconds: storage.short_term_seconds,
        medium_term_seconds: storage.medium_term_seconds,
        flush_interval_seconds: storage.flush_interval_seconds,
        write_buffer_bytes: usize_u64(storage.write_buffer_bytes),
        long_term_max_bytes: storage.long_term_max_bytes,
        catalog_bytes: storage.catalog_bytes,
        catalog: storage.catalog.map(|catalog| proto::CatalogHealthSnapshot {
            recording_files: catalog.recording_files,
            finalized_files: catalog.finalized_files,
            active_files: catalog.active_files,
            fragments: catalog.fragments,
            fragment_bytes: catalog.fragment_bytes,
            events: catalog.events,
            open_events: catalog.open_events,
            event_thumbnails: catalog.event_thumbnails,
            oldest_recording_at_ms: catalog.oldest_recording_at_ms,
            newest_recording_at_ms: catalog.newest_recording_at_ms,
            protected_files: catalog.protected_files,
            recording_bytes: catalog.recording_bytes,
        }),
        demand: Some(proto::RecordingDemandHealthSnapshot {
            active_streams: usize_u64(storage.demand.active_streams),
            total_viewers: usize_u64(storage.demand.total_viewers),
            leased_streams: usize_u64(storage.demand.leased_streams),
            streams: storage
                .demand
                .streams
                .into_iter()
                .map(|stream| proto::RecordingDemandStreamHealthSnapshot {
                    stream_id: stream.stream_id,
                    viewers: usize_u64(stream.viewers),
                    lease_remaining_ms: stream.lease_remaining_ms,
                })
                .collect(),
        }),
        minimum_free_bytes: storage.minimum_free_bytes,
        maximum_used_percent: storage.maximum_used_percent.map(u32::from),
        warning_free_bytes: storage.warning_free_bytes,
        critical_free_bytes: storage.critical_free_bytes,
        cleanup_hysteresis_bytes: storage.cleanup_hysteresis_bytes,
        safety: Some(proto::StorageSafetyHealthSnapshot {
            pressure: safety.pressure.as_str().to_owned(),
            recording_state: safety.recording_state.as_str().to_owned(),
            total_bytes: safety.total_bytes,
            available_bytes: safety.available_bytes,
            keeppeek_bytes: safety.keeppeek_bytes,
            effective_limit_bytes: safety.effective_limit_bytes,
            cleanup_target_bytes: safety.cleanup_target_bytes,
            warning_free_bytes: safety.warning_free_bytes,
            critical_free_bytes: safety.critical_free_bytes,
            recovery_free_bytes: safety.recovery_free_bytes,
            last_evaluation_at_ms: safety.last_evaluation_at_ms,
            last_evaluation_trigger: safety
                .last_evaluation_trigger
                .map(|trigger| trigger.as_str().to_owned()),
            cleanup_running: safety.cleanup_running,
            last_cleanup_started_at_ms: safety.last_cleanup_started_at_ms,
            last_cleanup_ended_at_ms: safety.last_cleanup_ended_at_ms,
            last_cleanup_files_removed: safety.last_cleanup_files_removed,
            last_cleanup_bytes_removed: safety.last_cleanup_bytes_removed,
            last_cleanup_reason: safety
                .last_cleanup_reason
                .map(|reason| reason.as_str().to_owned()),
            last_failure_at_ms: safety.last_failure_at_ms,
            last_failure: safety.last_failure,
            last_recovered_at_ms: safety.last_recovered_at_ms,
        }),
    }
}

fn proto_webrtc_health(health: crate::webrtc::WebRtcHealth) -> proto::WebRtcHealthSnapshot {
    proto::WebRtcHealthSnapshot {
        active_sessions: usize_u64(health.active_sessions),
        adaptive_sessions: usize_u64(health.adaptive_sessions),
        multi_track_sessions: usize_u64(health.multi_track_sessions),
        multi_tracks: usize_u64(health.multi_tracks),
        fixed_sessions: usize_u64(health.fixed_sessions),
        active_main: usize_u64(health.active_main),
        active_sub: usize_u64(health.active_sub),
        requested_auto: usize_u64(health.requested_auto),
        requested_high: usize_u64(health.requested_high),
        requested_low: usize_u64(health.requested_low),
        estimated_bitrate_min_bps: health.estimated_bitrate_min_bps,
        estimated_bitrate_avg_bps: health.estimated_bitrate_avg_bps,
        estimated_bitrate_max_bps: health.estimated_bitrate_max_bps,
        source_bitrate_bps: health.source_bitrate_bps,
        published_frames: health.published_frames,
        published_bytes: health.published_bytes,
        delivered_frames: health.delivered_frames,
        written_frames: health.written_frames,
        queue_capacity: usize_u64(health.queue_capacity),
        queued_frames: usize_u64(health.queued_frames),
        queue_depth_max: usize_u64(health.queue_depth_max),
        queue_high_water: usize_u64(health.queue_high_water),
        queue_drops: health.queue_drops,
        queue_discarded_frames: health.queue_discarded_frames,
        queue_recovery_drops: health.queue_recovery_drops,
        session_queues: health
            .session_queues
            .into_iter()
            .map(|queue| proto::WebRtcSessionQueueHealthSnapshot {
                session_id: queue.session_id.as_u64(),
                track_id: queue.track_id.map(|track_id| track_id.to_string()),
                camera_ip: queue.camera_ip.to_string(),
                stream: queue.stream.to_string(),
                depth: usize_u64(queue.depth),
                high_water: usize_u64(queue.high_water),
                written_frames: queue.written_frames,
                full_drops: queue.full_drops,
                discarded_frames: queue.discarded_frames,
                recovery_drops: queue.recovery_drops,
            })
            .collect(),
        sources: health
            .sources
            .into_iter()
            .map(|source| proto::WebRtcSourceHealthSnapshot {
                camera_ip: source.camera_ip.to_string(),
                stream: source.stream.to_string(),
                subscribers: usize_u64(source.subscribers),
                bitrate_bps: source.bitrate_bps,
                has_keyframe: source.has_keyframe,
                keyframe_age_ms: source.keyframe_age_ms,
            })
            .collect(),
    }
}

fn usize_u64(value: usize) -> u64 {
    value.try_into().unwrap_or(u64::MAX)
}

fn nonzero_u64(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn nonzero_f64(value: f64) -> Option<f64> {
    (value != 0.0).then_some(value)
}

pub(super) fn aggregate_video_streams(
    camera: &CameraEntry,
    streams: &[StreamHealthReport],
    totals: &mut HealthTotals,
    issues: &mut Vec<HealthIssue>,
) {
    let video_streams = streams
        .iter()
        .filter(|stream| stream.report.kind.starts_with("video_"))
        .collect::<Vec<_>>();
    for stream in &video_streams {
        totals.ingress_fps += stream.report.fps;
        totals.ingress_bitrate_bps = totals
            .ingress_bitrate_bps
            .saturating_add((stream.report.kbps * 1_000.0).max(0.0) as u64);
        totals.frames = totals
            .frames
            .saturating_add(stream.report.frames.unwrap_or(0));
        totals.keyframes = totals
            .keyframes
            .saturating_add(stream.report.keyframes.unwrap_or(0));
        totals.drops = totals
            .drops
            .saturating_add(stream.report.drops.unwrap_or(0));
        totals.errors = totals
            .errors
            .saturating_add(stream.report.errors.unwrap_or(0));
        totals.reconnects = totals
            .reconnects
            .saturating_add(stream.report.reconnects.unwrap_or(0));
    }

    for stream in video_streams {
        let expected_gap_ms =
            (stream.report.expected_fps > 0.0).then(|| 1_000.0 / stream.report.expected_fps);
        if stream.report.jitter_samples > 0
            && expected_gap_ms.is_some_and(|expected| stream.report.jitter_p99_ms > expected)
        {
            issues.push(HealthIssue {
                severity: "info".to_owned(),
                scope: camera
                    .info
                    .name
                    .clone()
                    .unwrap_or_else(|| camera.info.ip.clone()),
                message: format!(
                    "{} frame-arrival jitter P99 is {:.1} ms",
                    stream.report.kind, stream.report.jitter_p99_ms
                ),
                operational_event_id: None,
                timeline_start_ms: None,
                timeline_end_ms: None,
            });
        }
        if stream.report.gap_max_ms > 2_000.0 {
            issues.push(HealthIssue {
                severity: "warning".to_owned(),
                scope: camera
                    .info
                    .name
                    .clone()
                    .unwrap_or_else(|| camera.info.ip.clone()),
                message: format!(
                    "{} maximum frame gap is {:.0} ms",
                    stream.report.kind, stream.report.gap_max_ms
                ),
                operational_event_id: None,
                timeline_start_ms: None,
                timeline_end_ms: None,
            });
        }
        if stream.recent_drops > 0 || stream.recent_errors > 0 {
            issues.push(HealthIssue {
                severity: "info".to_owned(),
                scope: camera
                    .info
                    .name
                    .clone()
                    .unwrap_or_else(|| camera.info.ip.clone()),
                message: format!(
                    "{} recent drops {}, errors {}",
                    stream.report.kind, stream.recent_drops, stream.recent_errors
                ),
                operational_event_id: None,
                timeline_start_ms: None,
                timeline_end_ms: None,
            });
        }
    }
}
