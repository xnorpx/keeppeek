use crate::{
    camera_events::Evidence,
    health::{
        CameraHealth, CameraHealthDimensions, CameraHealthReason, CameraHealthState, LoadHealth,
        MemoryHealth, ProcessHealth, ServerHealthResponse, StorageHealth, SystemHealth,
    },
    stats::CameraHealthReport,
};

pub(super) fn health() -> ServerHealthResponse {
    ServerHealthResponse {
        status: "healthy".to_owned(),
        health_contract_version: 1,
        generated_at_ms: 1000,
        uptime_seconds: 42,
        version: "test",
        totals: Default::default(),
        system: system(),
        storage: storage(),
        webrtc: webrtc(),
        cameras: vec![camera()],
        issues: Vec::new(),
        operational_events: Vec::new(),
    }
}

pub(super) fn report(events: Option<Evidence>) -> CameraHealthReport {
    CameraHealthReport {
        ip: "192.0.2.10".parse().unwrap(),
        name: Some("untrusted-report-name".to_owned()),
        brand: None,
        port: 80,
        streams: Vec::new(),
        events,
    }
}

pub(super) fn evidence() -> Evidence {
    Evidence {
        mode: "onvif-pullpoint",
        state: "subscribed",
        pull_advertised: Some(true),
        pull_capable: false,
        pulls: 101,
        empty_pulls: 2,
        notifications: 103,
        parse_errors: 4,
        reconnects: 5,
        renewals: 106,
        renew_failures: 7,
        lease_ms: 12345,
        active: 3,
        deduplicated: 8,
        metadata_bytes: 4096,
        metadata_documents: 110,
        metadata_loss: 11,
        metadata_errors: 12,
        metadata_available: true,
        queue_drops: 13,
        delivery_stalls: 14,
        dropped: 15,
        snapshots: 116,
        snapshot_failures: 17,
        ..Default::default()
    }
}

fn camera() -> CameraHealth {
    CameraHealth {
        id: "front-door".to_owned(),
        ip: "192.0.2.10".to_owned(),
        name: "Front Door".to_owned(),
        manufacturer: None,
        model: None,
        firmware_version: None,
        backend: "onvif".to_owned(),
        transport: "tcp".to_owned(),
        state: CameraHealthState::Healthy,
        reason: CameraHealthReason::Healthy,
        reason_codes: Vec::new(),
        detail: String::new(),
        dimensions: dimensions(),
        lifecycle: None,
        last_error: None,
        configured_profiles: Vec::new(),
        streams: Vec::new(),
    }
}

fn dimensions() -> CameraHealthDimensions {
    CameraHealthDimensions {
        configured: true,
        expected: true,
        configured_video_streams: 0,
        connected_video_streams: None,
        reporting_video_streams: 0,
        fresh_video_streams: 0,
        decodable_video_streams: 0,
        configured_video_stream_ids: Vec::new(),
        connected_video_stream_ids: None,
        reporting_video_stream_ids: Vec::new(),
        fresh_video_stream_ids: Vec::new(),
        decodable_video_stream_ids: Vec::new(),
        transport_connected: None,
        latest_report_at_ms: None,
        report_age_ms: None,
        frames_fresh: None,
        decodable: None,
        recent_reconnects: 0,
        recent_drops: 0,
        recent_errors: 0,
        recording_requested: false,
        recording_video_streams: 0,
        recording_streams_progressing: 0,
        recording_video_stream_ids: Vec::new(),
        recording_progressing_stream_ids: Vec::new(),
        recording_progressing: None,
        recording_progress_age_ms: None,
        session_duration_ms: None,
        recorded_main_duration_ms: 0,
        recorded_sub_duration_ms: 0,
        recorded_total_duration_ms: 0,
        battery_configured: false,
        battery_registered: None,
        battery_last_seen_age_ms: None,
        battery_wake_pending_age_ms: None,
        battery_sleeping: None,
    }
}

fn system() -> SystemHealth {
    SystemHealth {
        host_name: None,
        os_name: None,
        os_version: None,
        kernel_version: None,
        architecture: "test",
        system_uptime_seconds: 0,
        boot_time_seconds: 0,
        logical_cores: 1,
        physical_cores: None,
        cpu_brand: None,
        system_cpu_percent: 0.0,
        process: process(),
        memory: MemoryHealth {
            total_bytes: 0,
            used_bytes: 0,
            available_bytes: 0,
            total_swap_bytes: 0,
            used_swap_bytes: 0,
        },
        load: LoadHealth {
            one_minute: 0.0,
            five_minutes: 0.0,
            fifteen_minutes: 0.0,
        },
        cpus: Vec::new(),
        network_egress_bps: 0,
        networks: Vec::new(),
        disks: Vec::new(),
        temperatures: Vec::new(),
    }
}

fn process() -> ProcessHealth {
    ProcessHealth {
        pid: 1,
        name: None,
        executable: None,
        working_directory: None,
        cpu_percent: None,
        cpu_capacity_percent: None,
        cpu_core_equivalents: None,
        resident_memory_bytes: None,
        memory_capacity_percent: None,
        virtual_memory_bytes: None,
        started_at_seconds: None,
        uptime_seconds: None,
        tasks: None,
        read_bytes_per_second: None,
        write_bytes_per_second: None,
        total_read_bytes: None,
        total_written_bytes: None,
    }
}

fn storage() -> StorageHealth {
    StorageHealth {
        medium_term_path: String::new(),
        long_term_path: String::new(),
        paths_are_same: true,
        short_term_seconds: 0,
        medium_term_seconds: 0,
        flush_interval_seconds: 0,
        write_buffer_bytes: 0,
        long_term_max_bytes: 0,
        minimum_free_bytes: 0,
        maximum_used_percent: None,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        cleanup_hysteresis_bytes: 0,
        catalog_bytes: None,
        catalog: None,
        safety: Default::default(),
        demand: crate::storage::demand::RecordingDemandHealth {
            active_streams: 0,
            total_viewers: 0,
            leased_streams: 0,
            streams: Vec::new(),
        },
    }
}

fn webrtc() -> crate::webrtc::WebRtcHealth {
    crate::webrtc::WebRtcHealth {
        active_sessions: 0,
        adaptive_sessions: 0,
        multi_track_sessions: 0,
        multi_tracks: 0,
        fixed_sessions: 0,
        active_main: 0,
        active_sub: 0,
        requested_auto: 0,
        requested_high: 0,
        requested_low: 0,
        estimated_bitrate_min_bps: None,
        estimated_bitrate_avg_bps: None,
        estimated_bitrate_max_bps: None,
        source_bitrate_bps: 0,
        published_frames: 0,
        published_bytes: 0,
        delivered_frames: 0,
        written_frames: 0,
        queue_capacity: 0,
        queued_frames: 0,
        queue_depth_max: 0,
        queue_high_water: 0,
        queue_drops: 0,
        queue_discarded_frames: 0,
        queue_recovery_drops: 0,
        session_queues: Vec::new(),
        sources: Vec::new(),
    }
}
