<script lang="ts">
	import DesktopHealthOverview from '$lib/components/DesktopHealthOverview.svelte';
	import type { ServerHealthResponse } from '$lib/types';
	import { diagnosisVisualHealth } from '../../e2e/fixtures/diagnosis';

	const health = {
		status: 'degraded',
		health_contract_version: 1,
		generated_at_ms: diagnosisVisualHealth.generated_at_ms,
		uptime_seconds: 21 * 86_400 + 6 * 3_600,
		version: '0.4.1-pre',
		totals: {
			configured_cameras: 43,
			connected_cameras: 42,
			fresh_cameras: 42,
			decodable_cameras: 42,
			recording_cameras: 42,
			recording_requested_cameras: 43,
			unknown_cameras: 0,
			configured_video_streams: 84,
			connected_video_streams: 82,
			fresh_video_streams: 82,
			decodable_video_streams: 82,
			recording_video_streams: 82,
			recording_requested_video_streams: 84,
			ingress_fps: 615,
			ingress_bitrate_bps: 318_000_000,
			frames: 18_400_000,
			keyframes: 184_000,
			drops: 184_024,
			errors: 1,
			reconnects: 30
		},
		system: {
			host_name: 'keeppeek.local',
			os_name: 'macOS',
			os_version: 'macOS 15.4',
			kernel_version: '24.4.0',
			architecture: 'arm64',
			system_uptime_seconds: 21 * 86_400 + 6 * 3_600,
			boot_time_seconds: 1_721_000_000,
			logical_cores: 12,
			physical_cores: 10,
			cpu_brand: 'Apple Silicon',
			system_cpu_percent: 34,
			process: {
				pid: 3201,
				name: 'keeppeek',
				executable: '/Applications/KeepPeek.app/keeppeek',
				working_directory: '/Library/Application Support/KeepPeek',
				cpu_percent: 408,
				cpu_capacity_percent: 34,
				cpu_core_equivalents: 4.08,
				resident_memory_bytes: 6_100_000_000,
				memory_capacity_percent: 25,
				virtual_memory_bytes: 8_000_000_000,
				started_at_seconds: 1_721_000_000,
				uptime_seconds: 21 * 86_400 + 6 * 3_600,
				tasks: 48,
				read_bytes_per_second: 0,
				write_bytes_per_second: 41_000_000,
				total_read_bytes: 0,
				total_written_bytes: 2_400_000_000_000
			},
			memory: {
				total_bytes: 24_000_000_000,
				used_bytes: 12_000_000_000,
				available_bytes: 12_000_000_000,
				total_swap_bytes: 0,
				used_swap_bytes: 0
			},
			load: { one_minute: 4.2, five_minutes: 3.8, fifteen_minutes: 3.4 },
			cpus: [],
			network_egress_bps: 18_000_000,
			networks: [],
			disks: [
				{
					name: 'recordings',
					kind: 'SSD',
					file_system: 'apfs',
					mount_point: '/recordings',
					total_bytes: 8_000_000_000_000,
					available_bytes: 3_200_000_000_000,
					used_bytes: 4_800_000_000_000,
					removable: false,
					stores_recordings: true
				}
			],
			temperatures: []
		},
		storage: {
			medium_term_path: '/recordings',
			long_term_path: '/recordings',
			paths_are_same: true,
			short_term_seconds: 120,
			medium_term_seconds: 1800,
			flush_interval_seconds: 60,
			write_buffer_bytes: 8192,
			long_term_max_bytes: 2_000_000_000_000,
			catalog_bytes: 8_000_000,
			catalog: null,
			demand: { active_streams: 42, total_viewers: 1, leased_streams: 42, streams: [] }
		},
		webrtc: {
			active_sessions: 7,
			adaptive_sessions: 7,
			browser_sessions: 4,
			browser_tracks: 8,
			fixed_sessions: 0,
			active_main: 1,
			active_sub: 7,
			requested_auto: 8,
			requested_high: 0,
			requested_low: 0,
			estimated_bitrate_min_bps: 4_000_000,
			estimated_bitrate_avg_bps: 8_000_000,
			estimated_bitrate_max_bps: 12_000_000,
			source_bitrate_bps: 318_000_000,
			published_frames: 18_400_000,
			published_bytes: 2_400_000_000_000,
			delivered_frames: 440_000,
			written_frames: 440_000,
			queue_capacity: 256,
			queued_frames: 0,
			queue_depth_max: 0,
			queue_high_water: 2,
			queue_drops: 0,
			queue_discarded_frames: 0,
			queue_recovery_drops: 0,
			session_queues: [],
			sources: []
		},
		cameras: diagnosisVisualHealth.cameras.slice(0, 4),
		issues: diagnosisVisualHealth.issues
	} satisfies ServerHealthResponse;

	const browser = {
		roundTripMs: 7,
		jitterMs: 2,
		packetLossPercent: 0,
		framesDropped: 0,
		connection: '1 peer · 8 tracks · ICE-lite',
		decoder: 'Hardware · H.264',
		presented: '14.9 fps of 15',
		quality: 'Auto · 7 low · 1 high'
	};
</script>

<main
	data-paper-scenario="health.desktop.overview"
	class="h-[1302px] w-[1440px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<DesktopHealthOverview {health} {browser} paperFrame />
</main>
