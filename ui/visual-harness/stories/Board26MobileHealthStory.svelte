<script lang="ts">
	import MobileHealthOverview from '$lib/components/MobileHealthOverview.svelte';
	import MobileNavigation from '$lib/components/MobileNavigation.svelte';
	import type { ServerHealthResponse } from '$lib/types';
	import MobileDeviceStatusBar from './MobileDeviceStatusBar.svelte';

	const health = {
		status: 'degraded',
		health_contract_version: 1,
		generated_at_ms: Date.parse('2026-08-18T06:00:00Z'),
		uptime_seconds: 76_400,
		version: '0.4.1',
		totals: {
			configured_cameras: 42,
			connected_cameras: 41,
			fresh_cameras: 41,
			decodable_cameras: 41,
			recording_cameras: 41,
			recording_requested_cameras: 42,
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
			drops: 184_000,
			errors: 0,
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
				uptime_seconds: 76_400,
				tasks: 48,
				read_bytes_per_second: 0,
				write_bytes_per_second: 39_750_000,
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
			disks: [],
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
			active_sessions: 1,
			adaptive_sessions: 1,
			browser_sessions: 1,
			browser_tracks: 4,
			fixed_sessions: 0,
			active_main: 1,
			active_sub: 3,
			requested_auto: 4,
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
		cameras: [
			{
				id: 'back-yard',
				ip: '192.168.1.83',
				name: 'Back Yard',
				manufacturer: 'Reolink',
				model: 'RLC-820A',
				firmware_version: 'v1',
				backend: 'retina',
				transport: 'udp',
				state: 'offline',
				reason: 'transport_disconnected',
				reason_codes: ['transport_disconnected'],
				detail: 'Camera transport is disconnected',
				lifecycle: 'reconnecting',
				last_error: 'Camera transport is disconnected. Recording is not progressing.',
				configured_profiles: [],
				streams: [
					{
						type: 'main',
						reconnects: 27,
						updated_at_ms: Date.parse('2026-08-18T04:23:07Z'),
						report_age_ms: 8_040_000
					}
				]
			},
			{
				id: 'porch',
				ip: '192.168.1.84',
				name: 'Porch',
				manufacturer: 'ONVIF',
				model: null,
				firmware_version: null,
				backend: 'retina',
				transport: 'udp',
				state: 'degraded',
				reason: 'ingress_drops',
				reason_codes: ['ingress_drops'],
				detail: 'Recent ingress frames were dropped',
				lifecycle: 'running',
				last_error: 'Switching to TCP usually fixes it',
				configured_profiles: [],
				streams: [
					{
						type: 'main',
						fps: 21.5,
						expected_fps: 25,
						updated_at_ms: Date.parse('2026-08-18T06:00:00Z'),
						report_age_ms: 0
					}
				]
			}
		],
		issues: [
			{ severity: 'critical', scope: 'Back Yard', message: 'transport is disconnected' },
			{ severity: 'warning', scope: 'Porch', message: 'dropping frames' },
			{ severity: 'warning', scope: 'Projected in 3 days', message: 'Retention below 7 days soon' },
			{ severity: 'info', scope: 'runtime', message: 'A server update is available' }
		]
	} satisfies ServerHealthResponse;
</script>

<main
	data-paper-scenario="health.mobile.overview"
	class="flex h-[844px] w-[390px] flex-col overflow-hidden rounded-lg border border-hairline-strong bg-ground [font-synthesis:none]"
>
	<MobileDeviceStatusBar />
	<MobileHealthOverview {health} paperFrame />
	<MobileNavigation pathname="/system-health" fixed={false} />
</main>
