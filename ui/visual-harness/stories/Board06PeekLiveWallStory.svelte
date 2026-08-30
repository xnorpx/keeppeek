<script lang="ts">
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import PeekCameraTile from '$lib/components/PeekCameraTile.svelte';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type {
		CameraHealth,
		CameraHealthDimensions,
		CameraListItem,
		StreamHealthDimensions
	} from '$lib/types';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';

	setLivePeer();

	const nowMs = Date.parse('2026-08-18T06:37:23Z');
	const cameraRows = [
		['front-door', 'Front Door', 'healthy'],
		['driveway', 'Driveway', 'healthy'],
		['porch', 'Porch', 'degraded'],
		['alley', 'Alley', 'stale'],
		['yard-ptz', 'Yard PTZ', 'healthy'],
		['back-yard', 'Back Yard', 'offline']
	] as const;
	const cameras: CameraListItem[] = cameraRows.map(([id, name]) => ({
		id,
		ip: `192.0.2.${id.length}`,
		name,
		manufacturer: null,
		model: null,
		firmware_version: null,
		is_reolink: false,
		capabilities: {
			ptz: id === 'yard-ptz',
			audio: false,
			events: true,
			recording: true,
			analytics: false,
			imaging: false,
			two_way_audio: false
		},
		profiles: []
	}));
	const healthById = new Map<string, CameraHealth>(
		cameraRows.map(([id, name, state]) => {
			const reportAgeMs =
				state === 'stale' ? 41_000 : state === 'offline' ? 8_056_000 : id === 'porch' ? 4_000 : 0;
			const reason =
				state === 'healthy'
					? 'healthy'
					: state === 'degraded'
						? 'ingress_drops'
						: state === 'stale'
							? 'stream_report_stale'
							: 'transport_disconnected';
			const detail =
				state === 'healthy'
					? 'Transport, media, keyframe, and recording evidence is current'
					: state === 'degraded'
						? '14% frames dropped'
						: state === 'stale'
							? 'Stream health report is stale'
							: 'Camera transport is disconnected';
			const transportConnected = state !== 'offline';
			const framesFresh = state === 'healthy' || state === 'degraded';
			const recordingProgressing = framesFresh;
			const recordingDurations =
				id === 'front-door'
					? {
							session_duration_ms: 600_000,
							recorded_main_duration_ms: 480_000,
							recorded_sub_duration_ms: 300_000,
							recorded_total_duration_ms: 780_000
						}
					: {};
			return [
				id,
				{
					id,
					ip: `192.0.2.${id.length}`,
					name,
					manufacturer: null,
					model: null,
					firmware_version: null,
					state,
					reason,
					reason_codes: [reason],
					detail,
					dimensions: {
						transport_connected: transportConnected,
						frames_fresh: framesFresh,
						decodable: framesFresh,
						recording_requested: true,
						recording_progressing: recordingProgressing,
						...recordingDurations
					} as CameraHealthDimensions,
					lifecycle: state === 'offline' ? 'Reconnecting' : 'Connected',
					last_error: state === 'offline' ? 'Not recording. No footage since 04:23.' : null,
					configured_profiles: [],
					streams: [
						{
							type: 'sub',
							fps: state === 'degraded' ? 11 : state === 'stale' || state === 'offline' ? 0 : 25,
							frames: state === 'degraded' ? 86 : state === 'offline' ? 0 : 1_000,
							drops: state === 'degraded' ? 14 : 0,
							updated_at_ms: nowMs - reportAgeMs,
							report_age_ms: reportAgeMs,
							state,
							reason,
							reason_codes: [reason],
							detail,
							dimensions: {
								expected: true,
								transport_connected: transportConnected,
								report_fresh: reportAgeMs <= 30_000,
								frames_fresh: framesFresh,
								decodable: framesFresh,
								recording_requested: true,
								recording_progressing: recordingProgressing
							} as StreamHealthDimensions
						}
					]
				}
			] as const;
		})
	);
</script>

<main
	data-paper-scenario="peek.desktop.live-wall"
	class="flex h-[860px] w-[1440px] shrink-0 overflow-hidden rounded-lg border border-hairline bg-ground [font-synthesis:none]"
>
	<DesktopPaperRail active="dashboard" paperFull />

	<section class="relative h-[858px] w-[1374px] shrink-0" aria-label="Dashboard live wall">
		<div data-peek-paper-grid class="absolute inset-0 flex flex-col gap-3 p-2">
			{#each [0, 1] as row (row)}
				<div data-peek-paper-row={row + 1} class="flex h-[390px] w-[1358px] shrink-0 gap-3">
					{#each cameras.slice(row * 3, row * 3 + 3) as camera (camera.id)}
						<div class="h-[390px] min-w-0 flex-1">
							<PeekCameraTile
								{camera}
								health={healthById.get(camera.id)}
								stream="sub"
								desktopPaperFrame
								compactNowMs={nowMs}
								compactTimeZone="UTC"
								onfocus={() => {}}
							/>
						</div>
					{/each}
				</div>
			{/each}
			<div
				data-peek-paper-overflow
				class="flex h-[38px] w-[1358px] shrink-0 items-center gap-3 pt-1"
			>
				<span class="shrink-0 font-mono text-[11px] leading-[14px] tracking-[0.1em] text-text-faint"
					>6 OF 6</span
				>
				<div class="flex h-[34px] flex-1 gap-1.5">
					{#each cameras as camera (camera.id)}<span
							class="min-w-0 flex-1 rounded-sm border {camera.id === 'porch'
								? 'border-activity'
								: 'border-hairline'} bg-video"
						></span>{/each}
				</div>
				<button
					type="button"
					class="inline-flex h-[30px] items-center gap-[7px] rounded-sm border border-hairline-strong bg-raised px-[11px] text-[13px]"
					disabled>All cameras shown</button
				>
			</div>
		</div>

		<header
			data-peek-paper-context
			class="absolute top-3 left-3 z-30 flex h-8 items-center overflow-hidden rounded-sm bg-video/70 text-white shadow-md ring-1 ring-white/10 backdrop-blur-md"
		>
			<h1
				class="flex h-full items-center border-r border-white/10 px-2 font-mono text-[9px] font-semibold text-white/55"
			>
				PEEK
			</h1>
			<button
				type="button"
				class="inline-flex h-full items-center gap-2 px-2.5 text-xs font-medium text-white/90"
			>
				<Grid2X2Icon class="size-3.5 text-white/55" />All cameras
			</button>
		</header>
	</section>
</main>
