<script lang="ts">
	import { resolve } from '$app/paths';
	import { presentCameraControl } from '$lib/camera-control';
	import CameraPtzControl from '$lib/components/CameraPtzControl.svelte';
	import LiveVideo from '$lib/components/LiveVideo.svelte';
	import type { CameraHealth, CameraListItem } from '$lib/types';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';
	import EllipsisIcon from '@lucide/svelte/icons/ellipsis';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import MicIcon from '@lucide/svelte/icons/mic';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import SettingsIcon from '@lucide/svelte/icons/settings-2';
	import SlidersIcon from '@lucide/svelte/icons/sliders-horizontal';
	import SpeakerIcon from '@lucide/svelte/icons/volume-2';

	export type MobileCameraMode = 'live' | 'ptz' | 'settings';

	type Props = {
		camera: CameraListItem;
		health: CameraHealth | null;
		stream: 'main' | 'sub';
		previewAvailable: boolean;
		catalogUrl?: string | null;
		commandTransportAvailable: boolean;
		mode: MobileCameraMode;
		paperFrame?: boolean;
		onmode?: (mode: MobileCameraMode) => void;
	};

	let {
		camera,
		health,
		stream,
		previewAvailable,
		catalogUrl = null,
		commandTransportAvailable,
		mode,
		paperFrame = false,
		onmode
	}: Props = $props();
	let control = $derived(presentCameraControl(camera, commandTransportAvailable));
	let videoStream = $derived(
		health?.streams.find((candidate) => candidate.type.includes(stream)) ??
			health?.streams[0] ??
			null
	);

	function profile(streamId: 'main' | 'sub') {
		return camera.profiles.find((candidate) => candidate.stream === streamId) ?? null;
	}

	function videoLabel(): string {
		const current = profile(stream);
		return (
			[current?.resolution, current?.encoding?.toUpperCase()].filter(Boolean).join(' · ') ||
			'Format unavailable'
		);
	}

	function formatBitrate(kbps: number | undefined): string {
		return kbps === undefined ? '—' : `${(kbps / 1_000).toFixed(1)} Mb/s`;
	}

	function portSummary(): string {
		const entries = [
			camera.ports?.rtsp && `RTSP ${camera.ports.rtsp}`,
			camera.ports?.onvif && `ONVIF ${camera.ports.onvif}`
		].filter(Boolean);
		return entries.length > 0 ? entries.join(' · ') : 'Not reported';
	}
</script>

<section
	data-mobile-camera-page={mode}
	class="flex w-full flex-col md:hidden"
	aria-label={`Mobile camera ${mode}`}
>
	<header
		class="flex h-[52px] shrink-0 items-center justify-between gap-3 border-b border-hairline px-4"
	>
		<div class="flex min-w-0 items-center gap-3">
			{#if mode === 'live'}
				<a
					href={resolve('/')}
					class="grid size-[18px] shrink-0 place-items-center"
					aria-label="Return to Peek"
				>
					<ChevronLeftIcon class="size-[18px]" strokeWidth={2} />
				</a>
			{:else}
				<button
					type="button"
					class="grid size-[18px] shrink-0 place-items-center"
					aria-label="Back to camera live view"
					onclick={() => onmode?.('live')}
				>
					<ChevronLeftIcon class="size-[18px]" strokeWidth={2} />
				</button>
			{/if}
			<div class="min-w-0">
				<h1 class="truncate text-lg leading-5 font-semibold">
					{camera.name ?? camera.id}{mode === 'ptz'
						? ' · PTZ'
						: mode === 'settings'
							? ' · Settings'
							: ''}
				</h1>
				{#if mode !== 'settings'}
					<p class="mt-0.5 font-mono text-2xs leading-3 text-text-faint uppercase">
						{mode === 'ptz'
							? 'Manual control · User allowed'
							: `${health?.state ?? 'unknown'} · Recording`}
					</p>
				{:else}
					<p class="mt-0.5 font-mono text-2xs leading-3 text-healthy">READ ONLY · API EVIDENCE</p>
				{/if}
			</div>
		</div>
		{#if mode === 'live'}
			{#if catalogUrl}
				<a
					href={catalogUrl}
					target="_blank"
					rel="noopener noreferrer"
					class="grid size-8 shrink-0 place-items-center rounded-sm border border-hairline text-text-faint focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					title="Open CCTV Database"
					aria-label={`Open ${[camera.manufacturer, camera.model].filter(Boolean).join(' ') || 'camera'} on CCTV Database`}
				>
					<ExternalLinkIcon class="size-[18px]" />
				</a>
			{:else}
				<EllipsisIcon class="size-[18px] text-text-faint" />
			{/if}
		{:else if mode === 'ptz'}
			<span
				class="rounded-full border border-hairline px-2.5 py-1 font-mono text-2xs leading-3 text-text-faint"
			>
				{Math.round(videoStream?.fps ?? profile(stream)?.framerate ?? 0)} FPS
			</span>
		{:else}
			<span class="font-mono text-2xs leading-3 text-text-faint">NO BULK SAVE API</span>
		{/if}
	</header>

	{#if mode === 'live'}
		<div class="flex h-[652px] shrink-0 flex-col gap-[14px] p-[15px]">
			<div class="relative h-[252px] shrink-0 overflow-hidden rounded-sm bg-video">
				{#if previewAvailable}
					<LiveVideo
						cameraId={camera.id}
						{stream}
						quality="auto"
						class="size-full overflow-hidden"
					/>
				{:else}
					<div class="grid size-full place-items-center text-center">
						<div>
							<RadioIcon class="mx-auto size-5 text-text-faint" />
							<p class="mt-2 text-xs text-text-muted">Live preview unavailable</p>
						</div>
					</div>
				{/if}
				<div
					class="pointer-events-none absolute inset-x-2.5 top-2.5 flex items-center justify-between font-mono text-2xs leading-3 text-white/70"
				>
					<span class="rounded-sm bg-video/80 px-2 py-1">● LIVE</span><span>{videoLabel()}</span>
				</div>
				<div
					class="pointer-events-none absolute inset-x-2.5 bottom-2.5 flex items-center justify-between font-mono text-2xs leading-3 text-white/60"
				>
					<span>LIVE</span><span>{formatBitrate(videoStream?.kbps)}</span>
				</div>
			</div>

			<div
				class="flex h-[53px] shrink-0 gap-px overflow-hidden rounded-sm border border-hairline bg-hairline"
			>
				<div class="flex w-[119px] flex-col gap-[3px] bg-surface p-2.5">
					<span class="font-mono text-2xs leading-3 text-text-faint">QUALITY</span><span
						class="text-xs-plus leading-4"
						>{profile('main')?.encoding?.toUpperCase() ?? 'Unknown'}</span
					>
				</div>
				<div class="flex w-[119px] flex-col gap-[3px] bg-surface p-2.5">
					<span class="font-mono text-2xs leading-3 text-text-faint">RECORDING</span><span
						class="text-xs-plus leading-4 text-healthy">Server managed</span
					>
				</div>
				<div class="flex w-[118px] flex-col gap-[3px] bg-surface p-2.5">
					<span class="font-mono text-2xs leading-3 text-text-faint">PTZ</span><span
						class="text-xs-plus leading-4">{control.showPtz ? 'Available' : 'Unavailable'}</span
					>
				</div>
			</div>

			<div class="flex h-[22px] shrink-0 items-center justify-between">
				<h2 class="text-lg leading-5 font-semibold">Recent at this camera</h2>
				<a href={resolve('/events')} class="text-xs-plus leading-4 text-primary-soft">All events</a>
			</div>
			<div
				class="grid h-[74px] shrink-0 place-items-center rounded-sm border border-hairline bg-surface text-center"
			>
				<div>
					<p class="text-sm leading-4 font-medium">Recent event evidence unavailable</p>
					<p class="mt-1 text-xs leading-[14px] text-text-muted">
						Open Events for server-backed results.
					</p>
				</div>
			</div>
		</div>

		<footer
			class="flex h-[76px] shrink-0 items-start justify-center gap-1 border-t border-hairline bg-surface pt-2.5"
		>
			{#each [{ mode: 'ptz' as const, label: 'PTZ', icon: SlidersIcon, enabled: control.showPtz }, { mode: null, label: 'Talk', icon: MicIcon, enabled: false }, { mode: null, label: 'Listen', icon: SpeakerIcon, enabled: false }, { mode: 'settings' as const, label: 'Settings', icon: SettingsIcon, enabled: true }] as action (action.label)}
				<button
					type="button"
					class="flex h-[50px] w-[83px] flex-col items-center gap-1 text-xs leading-[14px] {action.enabled
						? 'text-text-faint'
						: 'text-text-faint/40'}"
					disabled={!action.enabled}
					title={action.enabled ? action.label : `${action.label} control unavailable`}
					onclick={() => action.mode && onmode?.(action.mode)}
				>
					<span
						class="grid size-8 place-items-center rounded-sm {action.mode === 'ptz'
							? 'bg-primary text-on-primary'
							: 'border border-hairline-strong bg-raised'}"><action.icon class="size-4" /></span
					>
					{action.label}
				</button>
			{/each}
		</footer>
	{:else if mode === 'ptz'}
		<div class="flex h-[728px] shrink-0 flex-col gap-[14px] p-[15px]">
			<div class="relative h-[220px] shrink-0 overflow-hidden rounded-sm bg-video">
				{#if previewAvailable}
					<LiveVideo
						cameraId={camera.id}
						{stream}
						quality="auto"
						class="size-full overflow-hidden"
					/>
				{:else}
					<div class="grid size-full place-items-center">
						<RadioIcon class="size-5 text-text-faint" />
					</div>
				{/if}
				<div
					class="pointer-events-none absolute inset-x-2.5 bottom-2.5 flex justify-between font-mono text-2xs leading-3 text-white/60"
				>
					<span>LIVE</span><span
						>{stream.toUpperCase()} · {profile(stream)?.encoding?.toUpperCase() ?? 'UNKNOWN'}</span
					>
				</div>
			</div>
			<CameraPtzControl
				cameraId={camera.id}
				commandAvailable={control.commandAvailable}
				reason={control.reason}
				variant="mobile"
			/>
			<div
				class="flex h-[42px] shrink-0 items-center gap-2 rounded-sm border border-healthy/35 bg-healthy/10 px-3 text-xs leading-4 text-text-muted"
			>
				<span class="size-1.5 rounded-full bg-healthy"></span>Control stays available to User role.
			</div>
		</div>
	{:else}
		<div class="flex h-[674px] shrink-0 flex-col gap-[14px] p-4">
			<section class="h-[143px] shrink-0 rounded-md border border-hairline bg-surface p-[14px]">
				<div class="flex h-5 items-center justify-between">
					<h2 class="text-md leading-5 font-semibold">Connection</h2>
					<span class="font-mono text-2xs leading-3 text-healthy">CURRENT</span>
				</div>
				<dl class="mt-2 divide-y divide-hairline text-xs-plus leading-4">
					<div class="flex h-8 items-center justify-between">
						<dt class="text-text-muted">Address</dt>
						<dd class="font-mono">{camera.ip} · {portSummary()}</dd>
					</div>
					<div class="flex h-[41px] items-center justify-between">
						<dt class="text-text-muted">Sign-in</dt>
						<dd class="text-right font-mono">
							Secrets write-only<br /><span class="text-text-faint">Inheritance unknown</span>
						</dd>
					</div>
				</dl>
			</section>
			<section class="h-[142px] shrink-0 rounded-md border border-hairline bg-surface p-[14px]">
				<h2 class="h-5 text-md leading-5 font-semibold">Streams & roles</h2>
				{#each [profile('main'), profile('sub')] as current, index (index)}
					<div
						class="flex h-9 items-center justify-between border-b border-hairline last:border-b-0"
					>
						<span class="text-xs-plus leading-4">{index === 0 ? 'Main' : 'Sub'}</span><span
							class="font-mono text-xs leading-[14px] text-text-muted"
							>{current
								? `${current.resolution ?? '—'} · ${current.encoding?.toUpperCase() ?? '—'}`
								: 'Not reported'}</span
						>
					</div>
				{/each}
			</section>
			<section class="h-[120px] shrink-0 rounded-md border border-hairline bg-surface p-[14px]">
				<div class="flex h-5 items-center justify-between">
					<h2 class="text-md leading-5 font-semibold">Recording</h2>
					<span class="font-mono text-2xs leading-3 text-text-faint">SERVER MANAGED</span>
				</div>
				<div
					class="mt-2 grid h-[34px] grid-cols-3 overflow-hidden rounded-sm border border-hairline text-center text-xs leading-4"
				>
					<span class="grid place-items-center bg-raised">Continuous</span><span
						class="grid place-items-center text-text-faint">Events</span
					><span class="grid place-items-center text-text-faint">Off</span>
				</div>
				<p class="mt-2 text-xs leading-4 text-text-faint">
					Per-camera retention and inheritance are not exposed.
				</p>
			</section>
			<div class="flex h-[43px] shrink-0 items-center justify-between border-b border-hairline">
				<span class="text-sm leading-[18px]">Events, Audio, Advanced</span><span
					class="font-mono text-2xs leading-[14px] text-text-faint">SECTIONS BELOW</span
				>
			</div>
		</div>
		<footer
			class="flex h-[54px] shrink-0 items-center justify-center border-t border-hairline bg-surface font-mono text-xs leading-4 text-text-faint"
		>
			Read-only camera evidence
		</footer>
	{/if}
</section>
