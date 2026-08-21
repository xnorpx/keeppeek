<script lang="ts">
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import PeekCameraTile from '$lib/components/PeekCameraTile.svelte';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type { CameraHealth, CameraListItem } from '$lib/types';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';
	import SearchIcon from '@lucide/svelte/icons/search';

	setLivePeer();

	const nowMs = Date.parse('2026-08-18T06:37:23Z');
	const cameraRows = [
		['front-door', 'Front Door', 'online'],
		['driveway', 'Driveway', 'online'],
		['porch', 'Porch', 'degraded'],
		['alley', 'Alley', 'stale'],
		['yard-ptz', 'Yard PTZ', 'online'],
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
					lifecycle:
						state === 'stale' ? 'Attempt 3' : state === 'offline' ? 'Stopped' : 'Connected',
					last_error: state === 'offline' ? 'Not recording. No footage since 04:23.' : null,
					configured_profiles: [],
					streams: [
						{
							type: 'sub',
							fps: state === 'degraded' ? 11 : state === 'stale' || state === 'offline' ? 0 : 25,
							frames: state === 'degraded' ? 86 : state === 'offline' ? 0 : 1_000,
							drops: state === 'degraded' ? 14 : 0,
							updated_at_ms: nowMs - reportAgeMs,
							report_age_ms: reportAgeMs
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
	<DesktopPaperRail active="peek" paperFull />

	<section class="flex h-[858px] w-[1374px] shrink-0 flex-col" aria-label="Peek live wall">
		<header
			data-peek-paper-context
			class="flex h-[52px] w-[1374px] shrink-0 items-center gap-3.5 border-b border-hairline px-5"
		>
			<h1 class="text-base leading-5 font-semibold">Peek</h1>
			<span class="text-[13px] leading-4 text-text-muted">Live view</span>
			<span class="h-4 w-px bg-hairline"></span>
			<button
				type="button"
				class="inline-flex h-7 items-center gap-[7px] rounded-sm border border-hairline-strong bg-raised px-[11px] text-[13px]"
			>
				<Grid2X2Icon class="size-[13px] text-text-muted" />Front of house<ChevronRightIcon
					class="size-3 rotate-90 text-text-faint"
				/>
			</button>
			<span class="flex-1"></span>
			<label
				class="flex h-[34px] w-[210px] items-center gap-2 rounded-sm border border-hairline bg-raised px-[11px] text-text-faint"
			>
				<SearchIcon class="size-[13px]" /><input
					type="search"
					aria-label="Search cameras"
					placeholder="Search cameras"
					class="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-text-faint"
				/><kbd class="rounded-xs bg-surface px-[5px] py-0.5 font-mono text-[10px]">⌘K</kbd>
			</label>
			<span
				class="rounded-sm border border-hairline bg-raised px-[11px] py-[5px] font-mono text-[11px] tracking-[0.08em] text-text-muted"
				>AUTO</span
			>
			<button
				type="button"
				class="grid size-[30px] place-items-center rounded-sm border border-hairline bg-raised"
				aria-label="Edit layout"><Grid2X2Icon class="size-[15px] text-text-muted" /></button
			>
		</header>

		<div data-peek-paper-grid class="flex h-[774px] w-[1374px] shrink-0 flex-col gap-3 p-4">
			{#each [0, 1] as row (row)}
				<div data-peek-paper-row={row + 1} class="flex h-[340px] w-[1342px] shrink-0 gap-3">
					{#each cameras.slice(row * 3, row * 3 + 3) as camera (camera.id)}
						<div class="h-[340px] min-w-0 flex-1">
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
				class="flex h-[38px] w-[1342px] shrink-0 items-center gap-3 pt-1"
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

		<footer
			data-peek-paper-status
			class="flex h-8 w-[1374px] shrink-0 items-center gap-[18px] border-t border-hairline bg-surface px-4"
		>
			<span class="flex items-center gap-[7px] text-[13px]"
				><span class="size-1.5 rounded-full bg-activity"></span>1 camera offline · 1 degraded</span
			>
			<span class="h-3.5 w-px bg-hairline"></span>
			<span class="font-mono text-[11px] text-text-muted">CPU 34%</span>
			<span class="font-mono text-[11px] text-text-muted">RAM 6.1/32 GB</span>
			<span class="h-3.5 w-px bg-hairline"></span>
			<span class="font-mono text-[11px] text-text-muted">STORAGE 71% · 12d PROJECTED</span>
			<span class="flex-1"></span>
			<span class="font-mono text-[11px] text-text-muted">RX 18.4 Mb/s</span>
			<span class="flex items-center gap-[7px] text-[13px] text-text-muted"
				><span class="size-1.5 rounded-full bg-healthy"></span>Recorder healthy</span
			>
		</footer>
	</section>
</main>
