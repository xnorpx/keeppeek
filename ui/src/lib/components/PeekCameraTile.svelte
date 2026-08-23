<script lang="ts">
	import { resolve } from '$app/paths';
	import type { CameraHealth, CameraListItem } from '$lib/types';
	import { presentPeekCamera, reconcilePeekCameraPlayback } from '$lib/peek-camera';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import VideoOffIcon from '@lucide/svelte/icons/video-off';
	import FirstKeyframeState from './FirstKeyframeState.svelte';
	import LiveVideo from './LiveVideo.svelte';

	type Props = {
		camera: CameraListItem;
		health?: CameraHealth | null;
		stream: 'main' | 'sub';
		mobileFeatured?: boolean;
		desktopPaperFrame?: boolean;
		layoutMode?: boolean;
		layoutSelected?: boolean;
		layoutSize?: string;
		compactStatus?: boolean;
		compactLiveBorder?: 'hairline' | 'strong';
		compactNowMs?: number;
		compactTimeZone?: string;
		firstFrameElapsedMsOverride?: number;
		onfocus: (cameraId: string) => void;
		onlayoutpointerdown?: (event: PointerEvent) => void;
		onlayoutpointermove?: (event: PointerEvent) => void;
		onlayoutpointerup?: (event: PointerEvent) => void;
		onlayoutpointercancel?: (event: PointerEvent) => void;
		onlayoutlostpointercapture?: (event: PointerEvent) => void;
		onlayoutkeydown?: (event: KeyboardEvent) => void;
	};

	let {
		camera,
		health = null,
		stream,
		mobileFeatured = false,
		desktopPaperFrame = false,
		layoutMode = false,
		layoutSelected = false,
		layoutSize,
		compactStatus = false,
		compactLiveBorder = 'strong',
		compactNowMs = Date.now(),
		compactTimeZone,
		firstFrameElapsedMsOverride,
		onfocus,
		onlayoutpointerdown,
		onlayoutpointermove,
		onlayoutpointerup,
		onlayoutpointercancel,
		onlayoutlostpointercapture,
		onlayoutkeydown
	}: Props = $props();
	let healthPresentation = $derived(presentPeekCamera(camera, health));
	let hasRecentFrames = $state(false);
	let presentation = $derived(
		reconcilePeekCameraPlayback(healthPresentation, health?.state ?? null, hasRecentFrames)
	);
	let waitingForFirstFrame = $derived(
		healthPresentation.state === 'live' &&
			camera.profiles.some((profile) => profile.encoding !== null) &&
			!hasRecentFrames
	);
	let firstFrameElapsedMs = $state(0);
	let effectiveFirstFrameElapsedMs = $derived(firstFrameElapsedMsOverride ?? firstFrameElapsedMs);
	let label = $derived(camera.name ?? camera.id);
	let rendersVideo = $derived(healthPresentation.state !== 'offline');
	let canFocus = $derived(presentation.state === 'live' || presentation.state === 'degraded');
	let mobileSizeClass = $derived(
		compactStatus
			? 'h-full min-w-0 flex-1 basis-0'
			: desktopPaperFrame
				? 'size-full'
				: layoutMode
					? 'size-full'
					: mobileFeatured
						? 'col-span-2 aspect-video md:col-span-1'
						: 'aspect-[174/110] md:aspect-video'
	);
	let mobileCompactFlexClass = $derived(
		desktopPaperFrame || layoutMode || mobileFeatured ? 'flex' : 'hidden md:flex'
	);
	let mobileCompactInlineFlexClass = $derived(
		desktopPaperFrame || layoutMode || mobileFeatured ? 'inline-flex' : 'hidden md:inline-flex'
	);
	let mobileCompactBlockHiddenClass = $derived(
		desktopPaperFrame || layoutMode || mobileFeatured ? '' : 'hidden md:block'
	);
	let stateColor = $derived(
		presentation.state === 'live'
			? 'bg-healthy'
			: presentation.state === 'degraded'
				? 'bg-activity'
				: presentation.state === 'offline'
					? 'bg-live'
					: 'bg-text-muted'
	);
	let borderColor = $derived(
		layoutMode && layoutSelected
			? 'border-primary ring-1 ring-primary'
			: compactStatus && presentation.state === 'degraded'
				? 'border-2 border-activity'
				: compactStatus && presentation.state === 'offline'
					? 'border-2 border-live'
					: compactStatus
						? compactLiveBorder === 'hairline'
							? 'border-hairline'
							: 'border-hairline-strong'
						: presentation.state === 'degraded'
							? 'border-activity'
							: presentation.state === 'offline'
								? 'border-hairline-strong border-dashed'
								: 'border-hairline'
	);
	let tileSurface = $derived(presentation.state === 'offline' ? 'bg-surface' : 'bg-video');
	let headerSurface = $derived(
		presentation.state === 'offline' ? 'bg-raised text-foreground' : 'bg-video/75 text-white'
	);
	let compactObservedAtMs = $derived.by(() => {
		const latestStream = health?.streams.reduce<(typeof health.streams)[number] | null>(
			(latest, candidate) =>
				latest === null || candidate.report_age_ms < latest.report_age_ms ? candidate : latest,
			null
		);
		if (!latestStream) return null;
		return latestStream.updated_at_ms >= Date.UTC(2000, 0, 1)
			? latestStream.updated_at_ms
			: compactNowMs - latestStream.report_age_ms;
	});
	let compactObservedTime = $derived(formatCompactTime(compactObservedAtMs, true));
	let compactObservedMinute = $derived(formatCompactTime(compactObservedAtMs, false));
	let compactOfflineDuration = $derived(formatCompactDuration(compactObservedAtMs));
	let compactDegradedDetail = $derived(
		presentation.detail?.replace(/^(\d+)% frames dropped$/i, '$1% of frames dropped') ??
			'Stream health degraded'
	);

	$effect(() => {
		if (!waitingForFirstFrame || firstFrameElapsedMsOverride !== undefined) {
			firstFrameElapsedMs = 0;
			return;
		}
		const startedAt = performance.now();
		const updateElapsed = () => {
			firstFrameElapsedMs = Math.max(0, performance.now() - startedAt);
		};
		updateElapsed();
		const timer = window.setInterval(updateElapsed, 100);
		return () => window.clearInterval(timer);
	});

	function cameraHref(): string {
		return `${resolve('/camera')}?camera=${encodeURIComponent(camera.id)}`;
	}

	function formatCompactTime(timestampMs: number | null, includeSeconds: boolean): string {
		if (timestampMs === null) return '—';
		return new Intl.DateTimeFormat('en-GB', {
			hour: '2-digit',
			minute: '2-digit',
			second: includeSeconds ? '2-digit' : undefined,
			hour12: false,
			timeZone: compactTimeZone
		}).format(new Date(timestampMs));
	}

	function formatCompactDuration(timestampMs: number | null): string {
		if (timestampMs === null) return '';
		const elapsedMinutes = Math.max(0, Math.floor((compactNowMs - timestampMs) / 60_000));
		const hours = Math.floor(elapsedMinutes / 60);
		const minutes = elapsedMinutes % 60;
		return hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
	}
</script>

<article
	data-peek-camera={camera.id}
	data-peek-camera-state={presentation.state}
	data-peek-camera-size={layoutMode ? 'layout' : mobileFeatured ? 'featured' : 'compact'}
	class="group relative min-w-0 overflow-hidden rounded-lg border md:col-span-1 {tileSurface} {borderColor} {mobileSizeClass}"
>
	{#if rendersVideo}
		<LiveVideo
			cameraId={camera.id}
			{stream}
			showDiagnostics={!compactStatus && !layoutMode}
			diagnosticsLabel={!compactStatus && !layoutMode ? label : undefined}
			diagnosticsStatusClass={stateColor}
			onframeactivitychange={(active) => (hasRecentFrames = active)}
			class="size-full overflow-hidden rounded-[inherit]"
		/>
	{:else}
		<div class="absolute inset-0 bg-surface"></div>
	{/if}

	{#if compactStatus}
		<div
			data-peek-camera-region="compact-status"
			class="pointer-events-none absolute inset-0 z-20 flex flex-col justify-between p-3 {presentation.state ===
			'offline'
				? 'text-foreground'
				: 'text-white'}"
		>
			<div class="flex items-center justify-between gap-2 font-mono text-2xs">
				<span
					class="inline-flex h-[22px] items-center gap-1.5 self-start rounded-xs px-2 leading-3 tracking-[0.08em] text-white {presentation.state ===
					'offline'
						? 'w-28 bg-live'
						: presentation.state === 'degraded'
							? 'w-[71px] bg-[#A87310E6]'
							: 'w-12 bg-video/75'}"
				>
					{#if presentation.state === 'live'}
						<span class="size-[5px] rounded-full bg-white/85"></span><span class="tracking-[0.08em]"
							>REC</span
						>
					{:else if presentation.state === 'degraded'}
						<span class="tracking-[0.08em]">DEGRADED</span>
					{:else if presentation.state === 'offline'}
						<span class="tracking-[0.08em]">OFFLINE {compactOfflineDuration}</span>
					{:else}
						<span class="tracking-[0.08em]">RECONNECTING</span>
					{/if}
				</span>
				{#if presentation.state !== 'offline'}
					<span class="leading-3 text-[#FFFFFFD1]">{compactObservedTime}</span>
				{/if}
			</div>

			{#if presentation.state === 'offline'}
				<div class="space-y-1.5 text-center">
					<p class="text-lg leading-5 font-semibold">Not recording</p>
					<p class="font-mono text-2xs leading-3 tracking-caps text-live-text uppercase">
						No footage since {compactObservedMinute}
					</p>
					<span
						class="pointer-events-auto inline-flex h-[30px] w-[86px] items-center justify-center rounded-sm bg-primary text-sm leading-4 font-semibold text-on-primary"
					>
						Diagnose
					</span>
				</div>
			{/if}

			<div class="flex items-center justify-between gap-2">
				<div class="flex min-w-0 flex-1 flex-col gap-[5px]">
					<p class="truncate text-md leading-[18px] font-semibold">{label}</p>
					{#if presentation.state === 'degraded'}
						<p class="font-mono text-2xs leading-3 tracking-caps text-white/70 uppercase">
							{compactDegradedDetail}{presentation.recording ? ' · still recording' : ''}
						</p>
					{/if}
				</div>
				{#if presentation.state !== 'degraded'}
					<span
						class="shrink-0 font-mono text-2xs leading-3 {presentation.state === 'offline'
							? 'text-text-faint'
							: 'text-white/60'}"
					>
						{presentation.state === 'offline'
							? `LAST SEEN ${compactObservedTime}`
							: `${stream.toUpperCase()} · ${Math.round(presentation.fps ?? 0)}FPS`}
					</span>
				{/if}
			</div>
		</div>
	{:else}
		<div
			data-peek-camera-region="header"
			class="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-start justify-between gap-2 p-2.5"
		>
			{#if layoutMode}
				<span
					data-peek-camera-label
					class="flex min-w-0 items-center gap-2 rounded-sm px-2.5 py-1.5 text-xs font-medium {headerSurface}"
				>
					<span class="size-1.5 shrink-0 rounded-full {stateColor}"></span>
					<span class="truncate">{label}</span>
				</span>
			{/if}
			{#if layoutMode && layoutSize && layoutSelected}
				<span
					class="flex shrink-0 items-center rounded-sm bg-primary px-2.5 py-1.5 font-mono text-2xs font-semibold tracking-caps text-on-primary"
				>
					{layoutSize}
				</span>
			{:else if presentation.recording}
				<span
					class="shrink-0 items-center gap-1.5 rounded-full bg-video/75 px-2.5 py-1.5 font-mono text-2xs font-semibold tracking-caps text-white {mobileCompactFlexClass}"
				>
					<span class="size-1.5 rounded-full bg-white/85"></span>
					REC
				</span>
			{/if}
			{#if !layoutMode && !rendersVideo}
				<span
					data-peek-camera-label
					class="mr-8 ml-auto flex min-w-0 items-center gap-2 rounded-sm px-2.5 py-1.5 text-xs font-medium {headerSurface}"
				>
					<span class="size-1.5 shrink-0 rounded-full {stateColor}"></span>
					<span class="truncate">{label}</span>
				</span>
			{/if}
		</div>
	{/if}

	{#if waitingForFirstFrame}
		<FirstKeyframeState
			{label}
			elapsedMs={effectiveFirstFrameElapsedMs}
			class="absolute inset-0 z-20"
		/>
	{/if}

	{#if !compactStatus && presentation.state === 'degraded'}
		<div
			data-peek-camera-region="evidence"
			class="pointer-events-none absolute right-2.5 left-2.5 z-20 rounded-sm border border-activity bg-activity/15 font-medium text-white {mobileFeatured
				? 'bottom-9 px-2.5 py-2 text-xs'
				: 'bottom-2 px-2 py-1.5 text-2xs'} md:bottom-9 md:px-2.5 md:py-2 md:text-xs"
			role="status"
		>
			{#if mobileFeatured}<span>Degraded —</span>{:else}<span class="hidden md:inline"
					>Degraded —</span
				>{/if}{' '}{presentation.detail}
		</div>
	{:else if !compactStatus && presentation.state === 'reconnecting'}
		<div
			class="absolute inset-0 z-20 grid place-items-center px-3 pt-8 text-center md:px-6 md:pt-0"
		>
			<div data-peek-camera-region="evidence" class="space-y-1 text-white md:space-y-2">
				<RefreshCwIcon
					class="mx-auto size-5 text-text-muted {mobileCompactBlockHiddenClass}"
					strokeWidth={1.75}
				/>
				<p class="text-sm font-medium">Reconnecting…</p>
				<p class="text-xs text-white/70">{presentation.detail}</p>
			</div>
		</div>
	{:else if !compactStatus && presentation.state === 'offline'}
		<div
			class="absolute inset-0 z-20 grid place-items-center px-3 pt-8 text-center md:px-6 md:pt-0"
		>
			<div data-peek-camera-region="evidence" class="space-y-1 md:space-y-2">
				<VideoOffIcon
					class="mx-auto size-5 text-live {mobileCompactBlockHiddenClass}"
					strokeWidth={1.75}
				/>
				<p class="text-sm font-semibold text-foreground">Offline</p>
				<p class="text-xs text-text-muted {mobileCompactBlockHiddenClass}">
					{presentation.detail}
				</p>
				<a
					href={resolve('/system-health')}
					class="relative z-30 h-8 items-center rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {mobileCompactInlineFlexClass}"
				>
					Diagnose
				</a>
			</div>
		</div>
	{/if}

	{#if layoutMode}
		<button
			type="button"
			class="absolute inset-0 z-10 rounded-[inherit] focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
			aria-label={`Select ${label} layout tile`}
			aria-pressed={layoutSelected}
			onclick={() => onfocus(camera.id)}
			onpointerdown={onlayoutpointerdown}
			onpointermove={onlayoutpointermove}
			onpointerup={onlayoutpointerup}
			onpointercancel={onlayoutpointercancel}
			onlostpointercapture={onlayoutlostpointercapture}
			onkeydown={onlayoutkeydown}
		></button>
	{:else if canFocus}
		<button
			type="button"
			data-peek-focus={camera.id}
			class="absolute inset-0 z-10 rounded-[inherit] focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
			aria-label={`Focus ${label} live view`}
			onclick={() => onfocus(camera.id)}
		></button>
	{/if}

	{#if !layoutMode}
		<a
			href={cameraHref()}
			class="absolute top-2.5 right-2.5 z-30 hidden size-7 translate-y-9 place-items-center rounded-sm bg-video/75 text-white opacity-0 transition-opacity group-hover:opacity-100 focus:opacity-100 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:grid"
			title={`Open ${label} camera information`}
		>
			<CameraIcon class="size-3.5" strokeWidth={1.75} />
			<span class="sr-only">Open camera information</span>
		</a>
	{/if}

	{#if !compactStatus && (layoutMode || desktopPaperFrame)}
		<div
			data-peek-camera-region="footer"
			class="pointer-events-none absolute inset-x-0 bottom-0 z-20 items-end justify-between gap-2 p-2.5 font-mono text-2xs text-white/80 {mobileCompactFlexClass}"
		>
			{#if layoutMode && layoutSize}
				<span class="tracking-caps">
					{layoutSelected ? stream.toUpperCase() : `${layoutSize} · ${stream.toUpperCase()}`}
				</span>
				<span class="tracking-caps">{layoutSelected ? '16:9 LOCKED' : ''}</span>
			{:else}
				{#if desktopPaperFrame && compactObservedTime}<span>{compactObservedTime}</span>{/if}
				{#if desktopPaperFrame && camera.capabilities?.ptz}
					<span class="rounded-sm bg-primary px-2.5 py-1.5 font-semibold tracking-caps text-white"
						>PTZ</span
					>
				{/if}
			{/if}
		</div>
	{/if}
</article>
