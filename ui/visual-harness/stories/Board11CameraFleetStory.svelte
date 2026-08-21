<script lang="ts">
	import { presentCameraFleetRow } from '$lib/camera-fleet';
	import CameraFleetRow from '$lib/components/CameraFleetRow.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import type { CameraHealth, CameraListItem } from '$lib/types';
	import SearchIcon from '@lucide/svelte/icons/search';

	const names = [
		['front-door', 'Front Door'],
		['porch', 'Porch'],
		['back-yard', 'Back Yard'],
		['yard-ptz', 'Yard PTZ'],
		['driveway', 'Driveway'],
		['workshop', 'Workshop']
	] as const;
	const cameras: CameraListItem[] = names.map(([id, name], index) => ({
		id,
		ip: `192.0.2.${index + 41}`,
		name,
		manufacturer: index % 2 === 0 ? 'Reolink' : 'ONVIF',
		model: index % 2 === 0 ? 'RLC-811A' : 'DS-2CD2143',
		firmware_version: null,
		is_reolink: index % 2 === 0,
		backend: index % 2 === 0 ? 'Reolink' : 'ONVIF',
		transport: index % 2 === 0 ? 'Baichuan · TCP' : 'RTSP · TCP',
		capabilities: {
			ptz: id === 'yard-ptz',
			audio: true,
			events: true,
			recording: true,
			analytics: false,
			imaging: true,
			two_way_audio: false
		},
		profiles: [
			{
				name: 'Main',
				stream: 'main',
				encoding: 'h265',
				resolution: '3840x2160',
				framerate: 25
			},
			{
				name: 'Sub',
				stream: 'sub',
				encoding: 'h264',
				resolution: '640x360',
				framerate: 15
			}
		]
	}));
	const healthById = new Map<string, CameraHealth>(
		cameras.map((camera, index) => {
			const state = index === 1 ? 'degraded' : index === 2 ? 'offline' : 'online';
			return [
				camera.id,
				{
					id: camera.id,
					ip: camera.ip,
					name: camera.name ?? camera.id,
					manufacturer: camera.manufacturer,
					model: camera.model,
					firmware_version: null,
					backend: camera.backend,
					transport: camera.transport,
					state,
					lifecycle: state === 'offline' ? 'Stopped' : 'Connected',
					last_error: state === 'offline' ? 'Authentication failed' : null,
					configured_profiles: camera.profiles,
					streams: [
						{
							type: 'main',
							codec: 'h265',
							resolution: '3840x2160',
							fps: state === 'offline' ? 0 : 25,
							kbps: state === 'offline' ? undefined : 6_200,
							frames: state === 'offline' ? 0 : 86,
							drops: index === 1 ? 14 : 0,
							updated_at_ms: 1,
							report_age_ms: state === 'offline' ? 8_040_000 : 20
						}
					]
				}
			] as const;
		})
	);
	const rows = cameras.map((camera) => ({
		camera,
		presentation: presentCameraFleetRow(camera, healthById.get(camera.id) ?? null)
	}));
	const selectedIds = new Set(rows.slice(0, 3).map((row) => row.camera.id));
</script>

<main
	data-paper-scenario="cameras.desktop.fleet"
	class="flex h-[624px] w-[1440px] shrink-0 overflow-hidden rounded-lg border border-hairline bg-surface [font-synthesis:none]"
>
	<DesktopPaperRail active="cameras" />

	<section class="flex h-[622px] w-[1374px] shrink-0 flex-col" aria-label="Camera fleet">
		<header
			class="flex h-[52px] w-[1374px] shrink-0 items-center justify-between gap-4 border-b border-hairline px-5"
		>
			<div class="flex items-baseline gap-3">
				<h1 class="text-base leading-5 font-semibold">Cameras</h1>
				<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-muted">
					127 OF 127 SOURCES
				</p>
			</div>
			<div class="flex items-center gap-2.5">
				<label
					class="flex h-[30px] w-[280px] items-center gap-2 rounded-sm border border-hairline bg-raised px-2.5"
				>
					<SearchIcon class="size-3.5 text-text-faint" />
					<input
						type="search"
						aria-label="Search cameras"
						placeholder="Name, address, model…"
						class="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-text-faint"
					/>
				</label>
				<button
					type="button"
					class="inline-flex h-[30px] items-center gap-1.5 rounded-sm border border-hairline-strong bg-raised px-2.5 text-[13px]"
				>
					<span class="size-1.5 rounded-full bg-live"></span>Not healthy
					<span class="font-mono text-2xs text-text-faint">2</span>
				</button>
				<button
					type="button"
					class="h-[30px] rounded-sm border border-hairline px-2.5 text-[13px] text-text-faint"
					disabled
				>
					Group registry unavailable
				</button>
				<a
					href="/cameras/new"
					class="inline-flex h-[30px] items-center rounded-sm bg-primary px-3.5 text-[13px] font-semibold text-on-primary"
					>Add camera</a
				>
			</div>
		</header>

		<div data-camera-fleet-table class="flex h-[436px] w-[1374px] shrink-0 flex-col px-5">
			<div
				class="grid h-[34px] w-[1334px] shrink-0 grid-cols-[32px_20px_270px_140px_230px_150px_140px_120px_152px_80px] items-center border-b border-hairline-strong font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-faint"
			>
				<input type="checkbox" class="size-[13px]" aria-label="Select all visible cameras" />
				<span></span><span>CAMERA</span><span>TRANSPORT</span><span>STREAMS</span><span
					>RECORDING</span
				><span>THROUGHPUT</span><span>GB / DAY</span><span>LAST EVENT</span><span></span>
			</div>
			<div
				class="flex h-8 w-[1334px] shrink-0 items-center gap-2.5 pt-2 font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-muted"
			>
				<span>CONFIGURED CAMERAS</span><span class="h-px flex-1 bg-hairline"></span><span
					class="text-text-faint">5</span
				>
			</div>
			{#each rows.slice(0, 5) as row, index (row.camera.id)}
				<CameraFleetRow
					camera={row.camera}
					presentation={row.presentation}
					selected={selectedIds.has(row.camera.id)}
					tabindex={index === 0 ? 0 : -1}
					onselect={() => {}}
					onfocus={() => {}}
					onkeydown={() => {}}
					paperFrame
				/>
			{/each}
			<div
				class="flex h-[34px] w-[1334px] shrink-0 items-center gap-2.5 pt-2 font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-muted"
			>
				<span>MORE CAMERA SOURCES</span><span class="h-px flex-1 bg-hairline"></span><span
					class="text-text-faint">122</span
				>
			</div>
			<CameraFleetRow
				camera={rows[5].camera}
				presentation={rows[5].presentation}
				selected={false}
				tabindex={-1}
				onselect={() => {}}
				onfocus={() => {}}
				onkeydown={() => {}}
				paperFrame
			/>
		</div>

		<div class="h-11 w-[1374px] shrink-0"></div>
		<div class="flex h-[58px] w-[1374px] shrink-0 items-center justify-between px-5 pb-3.5">
			<div
				class="flex h-11 items-center gap-3 rounded-md border border-hairline-strong bg-raised px-4"
			>
				<span class="text-[13px] font-medium">3 selected</span><span
					class="h-5 w-px bg-hairline-strong"
				></span><span class="text-[13px] text-text-faint">Bulk operations unavailable</span>
			</div>
			<span class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-faint">
				VIRTUALISED · 56PX ROWS · RENDERS 24 AT A TIME
			</span>
		</div>
		<footer
			class="flex h-8 w-[1374px] shrink-0 items-center justify-between border-t border-hairline bg-ground px-5"
		>
			<span class="text-xs-plus text-text-muted">1 camera offline · 1 degraded</span>
			<span class="font-mono text-2xs text-text-faint"
				>LAST EVENT NOT REPORTED · RECORDER HEALTH OBSERVED</span
			>
		</footer>
	</section>
</main>
