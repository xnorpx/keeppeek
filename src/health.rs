use crate::storage::safety::{StorageSafetyHealthSnapshot, filesystem_capacity};
use crate::{
    api::{CameraLifecycle, ProfileSummary},
    operational_events::OperationalEvent,
    stats::StreamHealthReport,
    storage::{catalog::CatalogStats, demand::RecordingDemandHealth},
    webrtc::WebRtcHealth,
};
use serde::Serialize;
use std::{path::Path, time::Instant};
use sysinfo::{
    Components, CpuRefreshKind, Disks, MemoryRefreshKind, Networks, Pid, ProcessesToUpdate,
    RefreshKind, System,
};

pub(crate) const CAMERA_HEALTH_CONTRACT_VERSION: u32 = 1;
pub(crate) const STREAM_REPORT_FRESHNESS_THRESHOLD_MS: u64 = 30_000;
pub(crate) const OFFLINE_EVIDENCE_THRESHOLD_MS: u64 = 90_000;

/// Canonical camera and stream presentation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraHealthState {
    Starting,
    Healthy,
    Degraded,
    Stale,
    Reconnecting,
    Offline,
    Stopped,
    Unknown,
}

impl CameraHealthState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Stale => "stale",
            Self::Reconnecting => "reconnecting",
            Self::Offline => "offline",
            Self::Stopped => "stopped",
            Self::Unknown => "unknown",
        }
    }
}

/// Stable evidence code explaining a canonical health state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraHealthReason {
    Healthy,
    Starting,
    NotExpected,
    BatterySleeping,
    EvidenceUnavailable,
    TransportDisconnected,
    TransportReconnecting,
    TransportPartiallyConnected,
    NoStreamReport,
    StreamReportStale,
    FramesNotArriving,
    FramesBelowExpected,
    KeyframesMissing,
    IngressReconnects,
    IngressDrops,
    IngressErrors,
    RecordingNotProgressing,
}

impl CameraHealthReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Starting => "starting",
            Self::NotExpected => "not_expected",
            Self::BatterySleeping => "battery_sleeping",
            Self::EvidenceUnavailable => "evidence_unavailable",
            Self::TransportDisconnected => "transport_disconnected",
            Self::TransportReconnecting => "transport_reconnecting",
            Self::TransportPartiallyConnected => "transport_partially_connected",
            Self::NoStreamReport => "no_stream_report",
            Self::StreamReportStale => "stream_report_stale",
            Self::FramesNotArriving => "frames_not_arriving",
            Self::FramesBelowExpected => "frames_below_expected",
            Self::KeyframesMissing => "keyframes_missing",
            Self::IngressReconnects => "ingress_reconnects",
            Self::IngressDrops => "ingress_drops",
            Self::IngressErrors => "ingress_errors",
            Self::RecordingNotProgressing => "recording_not_progressing",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::Healthy => "Transport, media, keyframe, and recording evidence is current",
            Self::Starting => "Waiting for initial camera evidence",
            Self::NotExpected => "Camera media is not currently expected",
            Self::BatterySleeping => "Battery camera is registered and sleeping",
            Self::EvidenceUnavailable => "Required camera evidence is unavailable",
            Self::TransportDisconnected => "Camera transport is disconnected",
            Self::TransportReconnecting => "Camera transport is reconnecting",
            Self::TransportPartiallyConnected => "One or more camera transports are disconnected",
            Self::NoStreamReport => "No stream health report has been received",
            Self::StreamReportStale => "One or more stream health reports are stale",
            Self::FramesNotArriving => "One or more streams are not receiving fresh frames",
            Self::FramesBelowExpected => "One or more streams are below the expected frame rate",
            Self::KeyframesMissing => "One or more streams have no recent decodable keyframe",
            Self::IngressReconnects => "A stream reconnected during the latest evidence window",
            Self::IngressDrops => "Recent ingress frames were dropped",
            Self::IngressErrors => "Recent ingress errors were reported",
            Self::RecordingNotProgressing => "Requested recording writes are not progressing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CameraHealthEvidence {
    pub expected: bool,
    pub lifecycle: Option<CameraLifecycle>,
    pub startup_grace: bool,
    pub report_age_ms: Option<u64>,
    pub frames_fresh: Option<bool>,
    pub decodable: Option<bool>,
    pub frame_rate_healthy: Option<bool>,
    pub recent_reconnects: u64,
    pub recent_drops: u64,
    pub recent_errors: u64,
    pub recording_requested: bool,
    pub recording_progressing: Option<bool>,
    pub battery_sleeping: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CameraHealthProjection {
    pub state: CameraHealthState,
    pub reason: CameraHealthReason,
    pub reasons: Vec<CameraHealthReason>,
}

pub(crate) fn project_camera_health(evidence: &CameraHealthEvidence) -> CameraHealthProjection {
    let projection = |state, reason, mut reasons: Vec<CameraHealthReason>| {
        if !reasons.contains(&reason) {
            reasons.insert(0, reason);
        }
        CameraHealthProjection {
            state,
            reason,
            reasons,
        }
    };

    if !evidence.expected {
        return projection(
            CameraHealthState::Stopped,
            CameraHealthReason::NotExpected,
            Vec::new(),
        );
    }
    if evidence.battery_sleeping == Some(true) {
        return projection(
            CameraHealthState::Stopped,
            CameraHealthReason::BatterySleeping,
            Vec::new(),
        );
    }
    if matches!(
        evidence.lifecycle,
        Some(CameraLifecycle::Stopped | CameraLifecycle::ShuttingDown)
    ) {
        return projection(
            CameraHealthState::Stopped,
            CameraHealthReason::NotExpected,
            Vec::new(),
        );
    }
    let complete_healthy_evidence = evidence.lifecycle == Some(CameraLifecycle::Connected)
        && evidence
            .report_age_ms
            .is_some_and(|age| age <= STREAM_REPORT_FRESHNESS_THRESHOLD_MS)
        && evidence.frames_fresh == Some(true)
        && evidence.decodable == Some(true)
        && evidence.frame_rate_healthy == Some(true)
        && evidence.recent_reconnects == 0
        && evidence.recent_drops == 0
        && evidence.recent_errors == 0
        && (!evidence.recording_requested || evidence.recording_progressing == Some(true));
    if evidence.startup_grace && !complete_healthy_evidence {
        return projection(
            CameraHealthState::Starting,
            CameraHealthReason::Starting,
            Vec::new(),
        );
    }

    if evidence.lifecycle == Some(CameraLifecycle::Reconnecting) {
        return if evidence
            .report_age_ms
            .is_some_and(|age| age <= OFFLINE_EVIDENCE_THRESHOLD_MS)
        {
            projection(
                CameraHealthState::Reconnecting,
                CameraHealthReason::TransportReconnecting,
                Vec::new(),
            )
        } else {
            projection(
                CameraHealthState::Offline,
                CameraHealthReason::TransportDisconnected,
                Vec::new(),
            )
        };
    }

    if evidence.lifecycle.is_none() && evidence.report_age_ms.is_none() {
        return projection(
            CameraHealthState::Unknown,
            CameraHealthReason::EvidenceUnavailable,
            Vec::new(),
        );
    }

    if evidence.report_age_ms.is_none() {
        return projection(
            CameraHealthState::Stale,
            CameraHealthReason::NoStreamReport,
            Vec::new(),
        );
    }
    if evidence
        .report_age_ms
        .is_some_and(|age| age > STREAM_REPORT_FRESHNESS_THRESHOLD_MS)
    {
        return projection(
            CameraHealthState::Stale,
            CameraHealthReason::StreamReportStale,
            Vec::new(),
        );
    }
    if evidence.frames_fresh == Some(false) {
        return projection(
            CameraHealthState::Stale,
            CameraHealthReason::FramesNotArriving,
            Vec::new(),
        );
    }

    let mut degraded_reasons = Vec::new();
    if evidence.lifecycle == Some(CameraLifecycle::Degraded) {
        degraded_reasons.push(CameraHealthReason::TransportPartiallyConnected);
    }
    if evidence.decodable == Some(false) {
        degraded_reasons.push(CameraHealthReason::KeyframesMissing);
    }
    if evidence.recording_requested && evidence.recording_progressing == Some(false) {
        degraded_reasons.push(CameraHealthReason::RecordingNotProgressing);
    }
    if evidence.frame_rate_healthy == Some(false) {
        degraded_reasons.push(CameraHealthReason::FramesBelowExpected);
    }
    if evidence.recent_errors > 0 {
        degraded_reasons.push(CameraHealthReason::IngressErrors);
    }
    if evidence.recent_drops > 0 {
        degraded_reasons.push(CameraHealthReason::IngressDrops);
    }
    if evidence.recent_reconnects > 0 {
        degraded_reasons.push(CameraHealthReason::IngressReconnects);
    }
    if let Some(reason) = degraded_reasons.first().copied() {
        return projection(CameraHealthState::Degraded, reason, degraded_reasons);
    }

    let required_evidence_available = evidence.lifecycle.is_some()
        && evidence.frames_fresh.is_some()
        && evidence.decodable.is_some()
        && evidence.frame_rate_healthy.is_some()
        && (!evidence.recording_requested || evidence.recording_progressing.is_some());
    if !required_evidence_available {
        return projection(
            CameraHealthState::Unknown,
            CameraHealthReason::EvidenceUnavailable,
            Vec::new(),
        );
    }

    projection(
        CameraHealthState::Healthy,
        CameraHealthReason::Healthy,
        Vec::new(),
    )
}

/// Independent camera evidence used by health consumers and automation.
#[derive(Debug, Clone, Serialize)]
pub struct CameraHealthDimensions {
    pub configured: bool,
    pub expected: bool,
    pub configured_video_streams: usize,
    pub connected_video_streams: Option<usize>,
    pub reporting_video_streams: usize,
    pub fresh_video_streams: usize,
    pub decodable_video_streams: usize,
    pub configured_video_stream_ids: Vec<String>,
    pub connected_video_stream_ids: Option<Vec<String>>,
    pub reporting_video_stream_ids: Vec<String>,
    pub fresh_video_stream_ids: Vec<String>,
    pub decodable_video_stream_ids: Vec<String>,
    pub transport_connected: Option<bool>,
    pub latest_report_at_ms: Option<u64>,
    pub report_age_ms: Option<u64>,
    pub frames_fresh: Option<bool>,
    pub decodable: Option<bool>,
    pub recent_reconnects: u64,
    pub recent_drops: u64,
    pub recent_errors: u64,
    pub recording_requested: bool,
    pub recording_video_streams: usize,
    pub recording_streams_progressing: usize,
    pub recording_video_stream_ids: Vec<String>,
    pub recording_progressing_stream_ids: Vec<String>,
    pub recording_progressing: Option<bool>,
    pub recording_progress_age_ms: Option<u64>,
    pub session_duration_ms: Option<u64>,
    pub recorded_main_duration_ms: u64,
    pub recorded_sub_duration_ms: u64,
    pub recorded_total_duration_ms: u64,
    pub battery_configured: bool,
    pub battery_registered: Option<bool>,
    pub battery_last_seen_age_ms: Option<u64>,
    pub battery_wake_pending_age_ms: Option<u64>,
    pub battery_sleeping: Option<bool>,
}

/// Independent stream evidence used by health consumers and automation.
#[derive(Debug, Clone, Serialize)]
pub struct StreamHealthDimensions {
    pub expected: bool,
    pub transport_connected: Option<bool>,
    pub report_fresh: bool,
    pub report_freshness_threshold_ms: u64,
    pub frames_fresh: bool,
    pub frame_freshness_threshold_ms: u64,
    pub decodable: bool,
    pub keyframe_freshness_threshold_ms: u64,
    pub recent_reconnects: u64,
    pub recent_drops: u64,
    pub recent_errors: u64,
    pub recording_requested: bool,
    pub recording_progressing: Option<bool>,
    pub recording_progress_age_ms: Option<u64>,
    pub session_duration_ms: u64,
    pub recorded_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamHealth {
    #[serde(flatten)]
    pub(crate) ingress: StreamHealthReport,
    pub state: CameraHealthState,
    pub reason: CameraHealthReason,
    pub reason_codes: Vec<CameraHealthReason>,
    pub detail: String,
    pub dimensions: StreamHealthDimensions,
}

#[derive(Debug, Serialize)]
pub struct ServerHealthResponse {
    pub status: String,
    pub health_contract_version: u32,
    pub generated_at_ms: u64,
    pub uptime_seconds: u64,
    pub version: &'static str,
    pub totals: HealthTotals,
    pub system: SystemHealth,
    pub storage: StorageHealth,
    pub(crate) webrtc: WebRtcHealth,
    pub cameras: Vec<CameraHealth>,
    pub issues: Vec<HealthIssue>,
    pub(crate) operational_events: Vec<OperationalEvent>,
}

#[derive(Debug, Default, Serialize)]
pub struct HealthTotals {
    pub configured_cameras: usize,
    pub connected_cameras: usize,
    pub fresh_cameras: usize,
    pub decodable_cameras: usize,
    pub recording_requested_cameras: usize,
    pub recording_cameras: usize,
    pub unknown_cameras: usize,
    pub configured_video_streams: usize,
    pub connected_video_streams: usize,
    pub fresh_video_streams: usize,
    pub decodable_video_streams: usize,
    pub recording_requested_video_streams: usize,
    pub recording_video_streams: usize,
    pub ingress_fps: f64,
    pub ingress_bitrate_bps: u64,
    pub frames: u64,
    pub keyframes: u64,
    pub drops: u64,
    pub errors: u64,
    pub reconnects: u64,
}

#[derive(Debug, Serialize)]
pub struct CameraHealth {
    pub id: String,
    pub ip: String,
    pub name: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub firmware_version: Option<String>,
    pub backend: String,
    pub transport: String,
    pub state: CameraHealthState,
    pub reason: CameraHealthReason,
    pub reason_codes: Vec<CameraHealthReason>,
    pub detail: String,
    pub dimensions: CameraHealthDimensions,
    pub lifecycle: Option<String>,
    pub last_error: Option<String>,
    pub configured_profiles: Vec<ProfileSummary>,
    pub(crate) streams: Vec<StreamHealth>,
}

#[derive(Debug, Serialize)]
pub struct HealthIssue {
    pub severity: String,
    pub scope: String,
    pub message: String,
    pub operational_event_id: Option<String>,
    pub timeline_start_ms: Option<i64>,
    pub timeline_end_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct StorageHealth {
    pub medium_term_path: String,
    pub long_term_path: String,
    pub paths_are_same: bool,
    pub short_term_seconds: u64,
    pub medium_term_seconds: u64,
    pub flush_interval_seconds: u64,
    pub write_buffer_bytes: usize,
    pub long_term_max_bytes: u64,
    pub minimum_free_bytes: u64,
    pub maximum_used_percent: Option<u8>,
    pub warning_free_bytes: u64,
    pub critical_free_bytes: u64,
    pub cleanup_hysteresis_bytes: u64,
    pub catalog_bytes: Option<u64>,
    pub catalog: Option<CatalogStats>,
    pub(crate) safety: StorageSafetyHealthSnapshot,
    pub(crate) demand: RecordingDemandHealth,
}

#[derive(Debug, Serialize)]
pub struct SystemHealth {
    pub host_name: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    pub architecture: &'static str,
    pub system_uptime_seconds: u64,
    pub boot_time_seconds: u64,
    pub logical_cores: usize,
    pub physical_cores: Option<usize>,
    pub cpu_brand: Option<String>,
    pub system_cpu_percent: f32,
    pub process: ProcessHealth,
    pub memory: MemoryHealth,
    pub load: LoadHealth,
    pub cpus: Vec<CpuHealth>,
    pub network_egress_bps: u64,
    pub networks: Vec<NetworkHealth>,
    pub disks: Vec<DiskHealth>,
    pub temperatures: Vec<TemperatureHealth>,
}

#[derive(Debug, Serialize)]
pub struct ProcessHealth {
    pub pid: u32,
    pub name: Option<String>,
    pub executable: Option<String>,
    pub working_directory: Option<String>,
    /// Process CPU where one fully utilized logical core is 100%.
    pub cpu_percent: Option<f32>,
    /// Process CPU normalized to total logical CPU capacity, from 0% to 100%.
    pub cpu_capacity_percent: Option<f32>,
    /// Number of logical cores currently consumed by the process.
    pub cpu_core_equivalents: Option<f32>,
    /// Physical resident memory used by the process.
    pub resident_memory_bytes: Option<u64>,
    /// Resident process memory as a percentage of total host RAM.
    pub memory_capacity_percent: Option<f64>,
    /// Virtual address space reserved by the process.
    pub virtual_memory_bytes: Option<u64>,
    pub started_at_seconds: Option<u64>,
    pub uptime_seconds: Option<u64>,
    pub tasks: Option<usize>,
    pub read_bytes_per_second: Option<u64>,
    pub write_bytes_per_second: Option<u64>,
    pub total_read_bytes: Option<u64>,
    pub total_written_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct MemoryHealth {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub total_swap_bytes: u64,
    pub used_swap_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct LoadHealth {
    pub one_minute: f64,
    pub five_minutes: f64,
    pub fifteen_minutes: f64,
}

#[derive(Debug, Serialize)]
pub struct CpuHealth {
    pub name: String,
    pub usage_percent: f32,
    pub frequency_mhz: u64,
}

#[derive(Debug, Serialize)]
pub struct NetworkHealth {
    pub name: String,
    pub received_bytes_per_second: u64,
    pub transmitted_bytes_per_second: u64,
    pub received_packets_per_second: u64,
    pub transmitted_packets_per_second: u64,
    pub receive_errors: u64,
    pub transmit_errors: u64,
    pub total_received_bytes: u64,
    pub total_transmitted_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct DiskHealth {
    pub name: String,
    pub kind: String,
    pub file_system: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub removable: bool,
    pub stores_recordings: bool,
}

#[derive(Debug, Serialize)]
pub struct TemperatureHealth {
    pub label: String,
    pub current_celsius: Option<f32>,
    pub max_celsius: Option<f32>,
    pub critical_celsius: Option<f32>,
}

pub struct SystemMonitor {
    system: System,
    networks: Networks,
    disks: Disks,
    components: Components,
    pid: Pid,
    last_refresh: Instant,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        let pid = Pid::from_u32(std::process::id());
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        Self {
            system,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            pid,
            last_refresh: Instant::now(),
        }
    }

    pub fn snapshot(&mut self, recording_path: &Path) -> SystemHealth {
        let elapsed = self.last_refresh.elapsed().as_secs_f64().max(0.001);
        self.last_refresh = Instant::now();
        self.system.refresh_cpu_usage();
        self.system.refresh_cpu_frequency();
        self.system.refresh_memory();
        self.system
            .refresh_processes(ProcessesToUpdate::Some(&[self.pid]), true);
        self.networks.refresh(true);
        self.disks.refresh(true);
        self.components.refresh(true);

        let process = self.system.process(self.pid);
        let logical_cores = self.system.cpus().len();
        let process_cpu_percent = process.map(sysinfo::Process::cpu_usage);
        let (process_cpu_capacity_percent, process_cpu_core_equivalents) =
            normalized_process_cpu(process_cpu_percent, logical_cores);
        let total_memory_bytes = self.system.total_memory();
        let process_resident_memory_bytes = process.map(sysinfo::Process::memory);
        let process_memory_capacity_percent =
            process_memory_percent(process_resident_memory_bytes, total_memory_bytes);
        let process_disk = process.map(sysinfo::Process::disk_usage);
        let load = System::load_average();
        let mut networks = self
            .networks
            .iter()
            .map(|(name, network)| NetworkHealth {
                name: name.clone(),
                received_bytes_per_second: rate(network.received(), elapsed),
                transmitted_bytes_per_second: rate(network.transmitted(), elapsed),
                received_packets_per_second: rate(network.packets_received(), elapsed),
                transmitted_packets_per_second: rate(network.packets_transmitted(), elapsed),
                receive_errors: network.total_errors_on_received(),
                transmit_errors: network.total_errors_on_transmitted(),
                total_received_bytes: network.total_received(),
                total_transmitted_bytes: network.total_transmitted(),
            })
            .collect::<Vec<_>>();
        networks.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let network_egress_bps = network_egress_bitrate_bps(&networks);

        let recording_path = if recording_path.is_absolute() {
            recording_path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(recording_path)
        };
        let recording_mount = self
            .disks
            .list()
            .iter()
            .filter(|disk| recording_path.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().components().count())
            .map(|disk| disk.mount_point().to_path_buf());
        let mut disks = self
            .disks
            .list()
            .iter()
            .map(|disk| DiskHealth {
                name: disk.name().to_string_lossy().into_owned(),
                kind: format!("{:?}", disk.kind()).to_ascii_lowercase(),
                file_system: disk.file_system().to_string_lossy().into_owned(),
                mount_point: disk.mount_point().to_string_lossy().into_owned(),
                total_bytes: disk.total_space(),
                available_bytes: disk.available_space(),
                used_bytes: disk.total_space().saturating_sub(disk.available_space()),
                removable: disk.is_removable(),
                stores_recordings: recording_mount
                    .as_deref()
                    .is_some_and(|mount| mount == disk.mount_point()),
            })
            .collect::<Vec<_>>();
        if recording_mount.is_none()
            && let Some(recording_disk) = fallback_recording_disk(&recording_path)
        {
            disks.push(recording_disk);
        }
        disks.sort_unstable_by(|left, right| left.mount_point.cmp(&right.mount_point));

        let mut temperatures = self
            .components
            .list()
            .iter()
            .map(|component| TemperatureHealth {
                label: component.label().to_owned(),
                current_celsius: component.temperature(),
                max_celsius: component.max(),
                critical_celsius: component.critical(),
            })
            .collect::<Vec<_>>();
        temperatures.sort_unstable_by(|left, right| left.label.cmp(&right.label));

        SystemHealth {
            host_name: System::host_name(),
            os_name: System::name(),
            os_version: System::long_os_version(),
            kernel_version: System::kernel_version(),
            architecture: std::env::consts::ARCH,
            system_uptime_seconds: System::uptime(),
            boot_time_seconds: System::boot_time(),
            logical_cores,
            physical_cores: System::physical_core_count(),
            cpu_brand: self.system.cpus().first().map(|cpu| cpu.brand().to_owned()),
            system_cpu_percent: self.system.global_cpu_usage(),
            process: ProcessHealth {
                pid: self.pid.as_u32(),
                name: process.map(|process| process.name().to_string_lossy().into_owned()),
                executable: process
                    .and_then(sysinfo::Process::exe)
                    .map(|path| path.to_string_lossy().into_owned()),
                working_directory: process
                    .and_then(sysinfo::Process::cwd)
                    .map(|path| path.to_string_lossy().into_owned()),
                cpu_percent: process_cpu_percent,
                cpu_capacity_percent: process_cpu_capacity_percent,
                cpu_core_equivalents: process_cpu_core_equivalents,
                resident_memory_bytes: process_resident_memory_bytes,
                memory_capacity_percent: process_memory_capacity_percent,
                virtual_memory_bytes: process.map(sysinfo::Process::virtual_memory),
                started_at_seconds: process.map(sysinfo::Process::start_time),
                uptime_seconds: process.map(sysinfo::Process::run_time),
                tasks: process
                    .and_then(sysinfo::Process::tasks)
                    .map(std::collections::HashSet::len),
                read_bytes_per_second: process_disk.map(|disk| rate(disk.read_bytes, elapsed)),
                write_bytes_per_second: process_disk.map(|disk| rate(disk.written_bytes, elapsed)),
                total_read_bytes: process_disk.map(|disk| disk.total_read_bytes),
                total_written_bytes: process_disk.map(|disk| disk.total_written_bytes),
            },
            memory: MemoryHealth {
                total_bytes: total_memory_bytes,
                used_bytes: self.system.used_memory(),
                available_bytes: self.system.available_memory(),
                total_swap_bytes: self.system.total_swap(),
                used_swap_bytes: self.system.used_swap(),
            },
            load: LoadHealth {
                one_minute: load.one,
                five_minutes: load.five,
                fifteen_minutes: load.fifteen,
            },
            cpus: self
                .system
                .cpus()
                .iter()
                .map(|cpu| CpuHealth {
                    name: cpu.name().to_owned(),
                    usage_percent: cpu.cpu_usage(),
                    frequency_mhz: cpu.frequency(),
                })
                .collect(),
            network_egress_bps,
            networks,
            disks,
            temperatures,
        }
    }
}

fn fallback_recording_disk(recording_path: &Path) -> Option<DiskHealth> {
    let capacity = filesystem_capacity(recording_path, 0).ok()?;
    Some(DiskHealth {
        name: "recording filesystem".to_owned(),
        kind: "unknown".to_owned(),
        file_system: "unknown".to_owned(),
        mount_point: recording_path.to_string_lossy().into_owned(),
        total_bytes: capacity.total_bytes,
        available_bytes: capacity.available_bytes,
        used_bytes: capacity
            .total_bytes
            .saturating_sub(capacity.available_bytes),
        removable: false,
        stores_recordings: true,
    })
}

fn rate(value: u64, elapsed_seconds: f64) -> u64 {
    (value as f64 / elapsed_seconds).round() as u64
}

fn normalized_process_cpu(
    core_percent: Option<f32>,
    logical_cores: usize,
) -> (Option<f32>, Option<f32>) {
    let Some(core_percent) = core_percent.filter(|value| value.is_finite() && *value >= 0.0) else {
        return (None, None);
    };
    if logical_cores == 0 {
        return (None, None);
    }
    let core_equivalents = (core_percent / 100.0).min(logical_cores as f32);
    let capacity_percent = (core_equivalents / logical_cores as f32 * 100.0).clamp(0.0, 100.0);
    (Some(capacity_percent), Some(core_equivalents))
}

fn process_memory_percent(resident_bytes: Option<u64>, total_memory_bytes: u64) -> Option<f64> {
    if total_memory_bytes == 0 {
        return None;
    }
    resident_bytes.map(|resident_bytes| {
        (resident_bytes as f64 / total_memory_bytes as f64 * 100.0).clamp(0.0, 100.0)
    })
}

fn network_egress_bitrate_bps(networks: &[NetworkHealth]) -> u64 {
    networks
        .iter()
        .filter(|network| !is_loopback_interface(&network.name))
        .fold(0, |total, network| {
            total.saturating_add(network.transmitted_bytes_per_second.saturating_mul(8))
        })
}

fn is_loopback_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "lo"
        || name.contains("loopback")
        || name.strip_prefix("lo").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_recording_disk_reports_unenumerated_paths() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-health-capacity-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let recording_path = root.join("not-created/archive");

        let disk = fallback_recording_disk(&recording_path).unwrap();

        assert_eq!(disk.mount_point, recording_path.to_string_lossy());
        assert_eq!(disk.name, "recording filesystem");
        assert!(disk.total_bytes > 0);
        assert!(disk.available_bytes <= disk.total_bytes);
        assert!(disk.stores_recordings);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn healthy_camera_evidence() -> CameraHealthEvidence {
        CameraHealthEvidence {
            expected: true,
            lifecycle: Some(CameraLifecycle::Connected),
            startup_grace: false,
            report_age_ms: Some(1_000),
            frames_fresh: Some(true),
            decodable: Some(true),
            frame_rate_healthy: Some(true),
            recent_reconnects: 0,
            recent_drops: 0,
            recent_errors: 0,
            recording_requested: true,
            recording_progressing: Some(true),
            battery_sleeping: None,
        }
    }

    #[test]
    fn camera_health_projection_applies_evidence_precedence() {
        let healthy = healthy_camera_evidence();
        let cases = [
            (
                "healthy",
                healthy.clone(),
                CameraHealthState::Healthy,
                CameraHealthReason::Healthy,
            ),
            (
                "connected with stale reports",
                CameraHealthEvidence {
                    report_age_ms: Some(STREAM_REPORT_FRESHNESS_THRESHOLD_MS + 1),
                    ..healthy.clone()
                },
                CameraHealthState::Stale,
                CameraHealthReason::StreamReportStale,
            ),
            (
                "fresh but undecodable",
                CameraHealthEvidence {
                    decodable: Some(false),
                    ..healthy.clone()
                },
                CameraHealthState::Degraded,
                CameraHealthReason::KeyframesMissing,
            ),
            (
                "live but recording is not progressing",
                CameraHealthEvidence {
                    recording_progressing: Some(false),
                    ..healthy.clone()
                },
                CameraHealthState::Degraded,
                CameraHealthReason::RecordingNotProgressing,
            ),
            (
                "connected without reports",
                CameraHealthEvidence {
                    report_age_ms: None,
                    frames_fresh: None,
                    decodable: None,
                    frame_rate_healthy: None,
                    ..healthy.clone()
                },
                CameraHealthState::Stale,
                CameraHealthReason::NoStreamReport,
            ),
            (
                "recently reconnecting",
                CameraHealthEvidence {
                    lifecycle: Some(CameraLifecycle::Reconnecting),
                    ..healthy.clone()
                },
                CameraHealthState::Reconnecting,
                CameraHealthReason::TransportReconnecting,
            ),
            (
                "reconnecting without recent evidence",
                CameraHealthEvidence {
                    lifecycle: Some(CameraLifecycle::Reconnecting),
                    report_age_ms: Some(OFFLINE_EVIDENCE_THRESHOLD_MS + 1),
                    ..healthy.clone()
                },
                CameraHealthState::Offline,
                CameraHealthReason::TransportDisconnected,
            ),
            (
                "battery sleeping",
                CameraHealthEvidence {
                    battery_sleeping: Some(true),
                    ..healthy.clone()
                },
                CameraHealthState::Stopped,
                CameraHealthReason::BatterySleeping,
            ),
            (
                "no server evidence",
                CameraHealthEvidence {
                    lifecycle: None,
                    report_age_ms: None,
                    frames_fresh: None,
                    decodable: None,
                    frame_rate_healthy: None,
                    recording_progressing: None,
                    ..healthy
                },
                CameraHealthState::Unknown,
                CameraHealthReason::EvidenceUnavailable,
            ),
        ];

        for (name, evidence, expected_state, expected_reason) in cases {
            let projection = project_camera_health(&evidence);
            assert_eq!(projection.state, expected_state, "{name}");
            assert_eq!(projection.reason, expected_reason, "{name}");
            assert_eq!(projection.reasons.first(), Some(&expected_reason), "{name}");
        }
    }

    #[test]
    fn camera_health_projection_does_not_treat_partial_evidence_as_healthy() {
        let projection = project_camera_health(&CameraHealthEvidence {
            lifecycle: None,
            ..healthy_camera_evidence()
        });

        assert_eq!(projection.state, CameraHealthState::Unknown);
        assert_eq!(projection.reason, CameraHealthReason::EvidenceUnavailable);
    }

    #[test]
    fn camera_health_projection_honors_exact_freshness_boundaries() {
        let at_report_boundary = project_camera_health(&CameraHealthEvidence {
            report_age_ms: Some(STREAM_REPORT_FRESHNESS_THRESHOLD_MS),
            ..healthy_camera_evidence()
        });
        assert_eq!(at_report_boundary.state, CameraHealthState::Healthy);

        let at_reconnect_boundary = project_camera_health(&CameraHealthEvidence {
            lifecycle: Some(CameraLifecycle::Reconnecting),
            report_age_ms: Some(OFFLINE_EVIDENCE_THRESHOLD_MS),
            ..healthy_camera_evidence()
        });
        assert_eq!(at_reconnect_boundary.state, CameraHealthState::Reconnecting);

        let after_reconnect_boundary = project_camera_health(&CameraHealthEvidence {
            lifecycle: Some(CameraLifecycle::Reconnecting),
            report_age_ms: Some(OFFLINE_EVIDENCE_THRESHOLD_MS + 1),
            ..healthy_camera_evidence()
        });
        assert_eq!(after_reconnect_boundary.state, CameraHealthState::Offline);
    }

    #[test]
    fn camera_health_projection_keeps_transient_first_window_evidence_starting() {
        let transient = project_camera_health(&CameraHealthEvidence {
            startup_grace: true,
            frame_rate_healthy: Some(false),
            recent_reconnects: 1,
            ..healthy_camera_evidence()
        });
        assert_eq!(transient.state, CameraHealthState::Starting);
        assert_eq!(transient.reason, CameraHealthReason::Starting);

        let healthy = project_camera_health(&CameraHealthEvidence {
            startup_grace: true,
            ..healthy_camera_evidence()
        });
        assert_eq!(healthy.state, CameraHealthState::Healthy);
        assert_eq!(healthy.reason, CameraHealthReason::Healthy);
    }

    #[test]
    fn camera_health_projection_preserves_concurrent_degraded_reasons() {
        let projection = project_camera_health(&CameraHealthEvidence {
            lifecycle: Some(CameraLifecycle::Degraded),
            decodable: Some(false),
            frame_rate_healthy: Some(false),
            recent_reconnects: 1,
            recent_drops: 2,
            recent_errors: 3,
            recording_progressing: Some(false),
            ..healthy_camera_evidence()
        });

        assert_eq!(projection.state, CameraHealthState::Degraded);
        assert_eq!(
            projection.reason,
            CameraHealthReason::TransportPartiallyConnected
        );
        assert_eq!(
            projection.reasons,
            [
                CameraHealthReason::TransportPartiallyConnected,
                CameraHealthReason::KeyframesMissing,
                CameraHealthReason::RecordingNotProgressing,
                CameraHealthReason::FramesBelowExpected,
                CameraHealthReason::IngressErrors,
                CameraHealthReason::IngressDrops,
                CameraHealthReason::IngressReconnects,
            ]
        );
    }

    fn network(name: &str, transmitted_bytes_per_second: u64) -> NetworkHealth {
        NetworkHealth {
            name: name.to_owned(),
            received_bytes_per_second: 0,
            transmitted_bytes_per_second,
            received_packets_per_second: 0,
            transmitted_packets_per_second: 0,
            receive_errors: 0,
            transmit_errors: 0,
            total_received_bytes: 0,
            total_transmitted_bytes: 0,
        }
    }

    #[test]
    fn network_egress_excludes_loopback_and_sums_active_interfaces() {
        let networks = [
            network("en0", 1_000_000),
            network("utun4", 250_000),
            network("lo0", 9_000_000),
            network("Loopback Pseudo-Interface 1", 8_000_000),
        ];

        assert_eq!(network_egress_bitrate_bps(&networks), 10_000_000);
    }

    #[test]
    fn process_cpu_is_normalized_to_total_logical_capacity() {
        assert_eq!(
            normalized_process_cpu(Some(148.0), 8),
            (Some(18.5), Some(1.48))
        );
        assert_eq!(
            normalized_process_cpu(Some(1_900.0), 18),
            (Some(100.0), Some(18.0))
        );
        assert_eq!(normalized_process_cpu(Some(20.0), 0), (None, None));
        assert_eq!(normalized_process_cpu(Some(f32::NAN), 8), (None, None));
    }

    #[test]
    fn process_memory_is_normalized_to_total_host_ram() {
        assert_eq!(
            process_memory_percent(Some(536_870_912), 17_179_869_184),
            Some(3.125)
        );
        assert_eq!(process_memory_percent(Some(20), 10), Some(100.0));
        assert_eq!(process_memory_percent(None, 10), None);
        assert_eq!(process_memory_percent(Some(10), 0), None);
    }
}
