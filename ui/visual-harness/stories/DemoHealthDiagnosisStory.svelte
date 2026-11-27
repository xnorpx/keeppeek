<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import DesktopCameraDiagnosis from '$lib/components/DesktopCameraDiagnosis.svelte';
	import DesktopHealthOverview from '$lib/components/DesktopHealthOverview.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import { cameraDiagnosisEvidence } from '$lib/health-presentation';
	import type { ServerHealthResponse } from '$lib/types';
	import { diagnosisVisualHealth } from '../../e2e/fixtures/diagnosis';

	setCapabilityState(['keeppeek.runtime-config.v1']);

	const initialHealth: ServerHealthResponse = {
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
		cameras: diagnosisVisualHealth.cameras,
		issues: diagnosisVisualHealth.issues
	};

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

	let health = $state.raw<ServerHealthResponse>(initialHealth);
	let selectedCameraId = $state<string | null>(null);
	let statusMessage = $state<string | null>(null);

	let overviewHealth = $derived<ServerHealthResponse>({
		...health,
		cameras: health.cameras.slice(0, 4)
	});
	let activeEvidence = $derived(
		selectedCameraId === null ? null : cameraDiagnosisEvidence(health, selectedCameraId)
	);

	function handleNavigation(event: MouseEvent): void {
		const target = event.target;
		if (!(target instanceof Element)) return;
		const anchor = target.closest('a');
		if (!anchor) return;
		const href = anchor.getAttribute('href') ?? '';
		event.preventDefault();
		const cameraMatch = /^\/system-health\/camera\/([^/?#]+)/.exec(href);
		if (cameraMatch) {
			selectedCameraId = decodeURIComponent(cameraMatch[1]);
			statusMessage = null;
			return;
		}
		if (href === '/system-health') {
			selectedCameraId = null;
			statusMessage = null;
		}
	}

	function switchToTcp(): void {
		const currentId = selectedCameraId;
		if (currentId === null) return;
		health = {
			...health,
			cameras: health.cameras.map((camera) =>
				camera.id === currentId ? { ...camera, transport: 'tcp' } : camera
			)
		};
		statusMessage = 'Transport changed to TCP.';
	}
</script>

<main
	data-paper-scenario="health.desktop.overview"
	data-demo-health-diagnosis
	class="h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<!-- ponytail: Scales the 1440x900 desktop Paper layout to 1280x800 so production column widths stay unclipped. -->
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="flex h-[900px] w-[1440px] origin-top-left overflow-hidden bg-surface"
		style="transform: scale(calc(1280 / 1440));"
		onclick={handleNavigation}
	>
		<DesktopPaperRail active="health" />

		<section
			class="flex h-[900px] w-[1376px] shrink-0 flex-col overflow-hidden bg-ground"
			aria-label={activeEvidence ? 'Camera diagnosis' : 'System health overview'}
		>
			{#if activeEvidence}
				<DesktopCameraDiagnosis
					evidence={activeEvidence}
					runtimeConfigSupported
					{statusMessage}
					onswitchtotcp={switchToTcp}
				/>
			{:else}
				<header
					class="flex h-[52px] shrink-0 items-center justify-between border-b border-hairline bg-surface px-5"
				>
					<div class="flex items-baseline gap-3">
						<h1 class="text-base font-semibold">Health</h1>
						<span class="font-mono text-2xs text-text-muted">UPTIME 21d 06h · v0.4.1-pre</span>
					</div>
					<div class="flex items-center gap-2.5">
						<span class="font-mono text-2xs tracking-caps text-text-faint">
							CONTRACT V1 · SERVER AUTHORITATIVE
						</span>
					</div>
				</header>
				<div class="min-h-0 flex-1 overflow-hidden px-8 py-6">
					<DesktopHealthOverview health={overviewHealth} {browser} />
				</div>
			{/if}
		</section>
	</div>
</main>
