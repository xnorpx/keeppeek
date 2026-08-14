use crate::{
    api::ProfileSummary,
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

#[derive(Debug, Serialize)]
pub struct ServerHealthResponse {
    pub status: String,
    pub generated_at_ms: u64,
    pub uptime_seconds: u64,
    pub version: &'static str,
    pub totals: HealthTotals,
    pub system: SystemHealth,
    pub storage: StorageHealth,
    pub(crate) webrtc: WebRtcHealth,
    pub cameras: Vec<CameraHealth>,
    pub issues: Vec<HealthIssue>,
}

#[derive(Debug, Default, Serialize)]
pub struct HealthTotals {
    pub configured_cameras: usize,
    pub reporting_cameras: usize,
    pub configured_video_streams: usize,
    pub reporting_video_streams: usize,
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
    pub state: String,
    pub lifecycle: Option<String>,
    pub last_error: Option<String>,
    pub configured_profiles: Vec<ProfileSummary>,
    pub(crate) streams: Vec<StreamHealthReport>,
}

#[derive(Debug, Serialize)]
pub struct HealthIssue {
    pub severity: String,
    pub scope: String,
    pub message: String,
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
    pub catalog_bytes: Option<u64>,
    pub catalog: Option<CatalogStats>,
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
                stores_recordings: recording_path.starts_with(disk.mount_point()),
            })
            .collect::<Vec<_>>();
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
