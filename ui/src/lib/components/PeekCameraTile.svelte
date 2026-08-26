<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { observeGridVisibility, type GridTileVisibility } from '$lib/grid-visibility';
	import type { CameraHealth, CameraListItem } from '$lib/types';
	import { presentPeekCamera } from '$lib/peek-camera';
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
		onframeactivitychange?: (cameraId: string, active: boolean) => void;
		onvisibilitychange?: (visibility: GridTileVisibility) => void;
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
		onframeactivitychange,
		onvisibilitychange,
		onlayoutpointerdown,
		onlayoutpointermove,
		onlayoutpointerup,
		onlayoutpointercancel,
		onlayoutlostpointercapture,
		onlayoutkeydown
	}: Props = $props();
	let tileElement: HTMLElement | null = $state(null);
	let presentation = $derived(presentPeekCamera(camera, health));
	let hasRecentFrames = $state(false);
	let waitingForFirstFrame = $derived(
		presentation.state === 'healthy' &&
			camera.profiles.some((profile) => profile.encoding !== null) &&
			!hasRecentFrames
	);
	let firstFrameElapsedMs = $state(0);
	let effectiveFirstFrameElapsedMs = $derived(firstFrameElapsedMsOverride ?? firstFrameElapsedMs);
	let visualState = $derived(presentation.state);
	let canonicalStateLabel = $derived(presentation.state.toUpperCase());
	let label = $derived(camera.name ?? camera.id);
	let rendersVideo = $derived(presentation.state !== 'offline' && presentation.state !== 'stopped');
	let canFocus = $derived(
		visualState === 'starting' ||
			visualState === 'healthy' ||
			visualState === 'degraded' ||
			visualState === 'stale' ||
			visualState === 'reconnecting'
	);
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
		visualState === 'healthy'
			? 'bg-healthy'
			: visualState === 'degraded' || visualState === 'stale' || visualState === 'reconnecting'
				? 'bg-activity'
				: visualState === 'offline'
					? 'bg-live'
					: 'bg-text-muted'
	);
	let borderColor = $derived(
		layoutMode && layoutSelected
			? 'border-primary ring-1 ring-primary'
			: compactStatus &&
				  (visualState === 'degraded' || visualState === 'stale' || visualState === 'reconnecting')
				? 'border-2 border-activity'
				: compactStatus && visualState === 'offline'
					? 'border-2 border-live'
					: compactStatus
						? compactLiveBorder === 'hairline'
							? 'border-hairline'
							: 'border-hairline-strong'
						: visualState === 'degraded' ||
							  visualState === 'stale' ||
							  visualState === 'reconnecting'
							? 'border-activity'
							: visualState === 'offline'
								? 'border-hairline-strong border-dashed'
								: 'border-hairline'
	);
	let tileSurface = $derived(
		presentation.state === 'offline' || presentation.state === 'stopped' ? 'bg-surface' : 'bg-video'
	);
	let headerSurface = $derived(
		presentation.state === 'offline' || presentation.state === 'stopped'
			? 'bg-raised text-foreground'
			: 'bg-video/75 text-white'
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
	let compactStateDetail = $derived(
		presentation.detail?.replace(/^(\d+)% frames dropped$/i, '$1% of frames dropped') ??
			'Camera health evidence unavailable'
	);
	let recordingDiagnostics = $derived.by(() => {
		const dimensions = health?.dimensions;
		if (!dimensions) {
			return {
				state: 'unknown' as const,
				detail: 'Not reported',
				sessionDurationMs: null,
				mainDurationMs: null,
				subDurationMs: null,
				totalDurationMs: null
			};
		}
		const durations = {
			sessionDurationMs: dimensions.session_duration_ms ?? null,
			mainDurationMs: dimensions.recorded_main_duration_ms ?? null,
			subDurationMs: dimensions.recorded_sub_duration_ms ?? null,
			totalDurationMs: dimensions.recorded_total_duration_ms ?? null
		};
		if (!dimensions.recording_requested) {
			return { state: 'off' as const, detail: 'Off', ...durations };
		}

		const requestedStreams = recordingStreamIds(
			dimensions.recording_video_stream_ids,
			(stream) => stream.dimensions?.recording_requested === true
		);
		const progressingStreams = recordingStreamIds(
			dimensions.recording_progressing_stream_ids,
			(stream) => stream.dimensions?.recording_progressing === true
		);
		if (progressingStreams.length > 0) {
			const pendingStreams = requestedStreams.filter(
				(streamId) => !progressingStreams.includes(streamId)
			);
			if (pendingStreams.length > 0) {
				return {
					state: 'not-progressing' as const,
					detail: `${formatRecordingStreams(progressingStreams)} recording · ${formatRecordingStreams(pendingStreams)} not progressing`,
					...durations
				};
			}
			return {
				state: 'recording' as const,
				detail: `${formatRecordingStreams(progressingStreams)} · recording`,
				...durations
			};
		}

		const streamLabel = formatRecordingStreams(requestedStreams);
		if (dimensions.recording_progressing === true) {
			return { state: 'recording' as const, detail: `${streamLabel} · recording`, ...durations };
		}
		if (dimensions.recording_progressing === false) {
			return {
				state: 'not-progressing' as const,
				detail: `${streamLabel} · not progressing`,
				...durations
			};
		}
		return { state: 'pending' as const, detail: `${streamLabel} · status pending`, ...durations };
	});

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

	onMount(() => {
		if (!tileElement || !onvisibilitychange) return;
		return observeGridVisibility(tileElement, camera.id, onvisibilitychange);
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

	function formatRecordingStreams(streamIds: readonly string[]): string {
		const names = [...new Set(streamIds.map((streamId) => streamId.replace(/^video_/, '')))].map(
			(streamId) =>
				streamId === 'main'
					? 'Main'
					: streamId === 'sub'
						? 'Sub'
						: streamId.charAt(0).toUpperCase() + streamId.slice(1)
		);
		if (names.length === 0) return 'Stream not reported';
		return `${names.join(' + ')} stream${names.length === 1 ? '' : 's'}`;
	}

	function recordingStreamIds(
		aggregatedIds: readonly string[] | undefined,
		matches: (stream: CameraHealth['streams'][number]) => boolean
	): string[] {
		if (aggregatedIds && aggregatedIds.length > 0) return [...aggregatedIds];
		return health?.streams.filter(matches).map((stream) => stream.type) ?? [];
	}

	function handleFrameActivity(active: boolean): void {
		hasRecentFrames = active;
		onframeactivitychange?.(camera.id, active);
	}
</script>

<article
	bind:this={tileElement}
	data-peek-camera={camera.id}
	data-peek-camera-state={visualState}
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
			diagnosticsRecording={recordingDiagnostics}
			onframeactivitychange={handleFrameActivity}
			class="size-full overflow-hidden rounded-[inherit]"
		/>
	{:else}
		<div class="absolute inset-0 bg-surface"></div>
	{/if}

	{#if compactStatus}
		<div
			data-peek-camera-region="compact-status"
			class="pointer-events-none absolute inset-0 z-20 flex flex-col justify-between p-3 {presentation.state ===
				'offline' || presentation.state === 'stopped'
				? 'text-foreground'
				: 'text-white'}"
		>
			<div class="flex items-center justify-between gap-2 font-mono text-2xs">
				<span
					class="inline-flex h-[22px] min-w-[76px] items-center justify-center gap-1.5 self-start rounded-xs px-2 leading-3 tracking-[0.08em] text-white {presentation.state ===
					'offline'
						? 'min-w-28 bg-live'
						: presentation.state === 'healthy'
							? 'bg-[#237A58E6]'
							: presentation.state === 'unknown' || presentation.state === 'stopped'
								? 'bg-text-muted'
								: 'bg-[#A87310E6]'}"
				>
					<span class="tracking-[0.08em]">
						{canonicalStateLabel}
					</span>
				</span>
				{#if presentation.state !== 'offline'}
					<span class="leading-3 text-[#FFFFFFD1]">{compactObservedTime}</span>
				{/if}
			</div>

			{#if presentation.state === 'offline'}
				<div class="space-y-1.5 text-center">
					<p class="text-lg leading-5 font-semibold">
						{presentation.detail ?? 'Camera transport is offline'}
					</p>
					<p class="font-mono text-2xs leading-3 tracking-caps text-live-text uppercase">
						Last report {compactObservedMinute}
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
					{#if presentation.state !== 'healthy' && presentation.state !== 'offline'}
						<p class="font-mono text-2xs leading-3 tracking-caps text-white/70 uppercase">
							{compactStateDetail}
						</p>
					{/if}
				</div>
				{#if presentation.state === 'healthy' || presentation.state === 'offline'}
					<span
						class="shrink-0 font-mono text-2xs leading-3 {presentation.state === 'offline'
							? 'text-text-faint'
							: 'text-white/60'}"
					>
						{presentation.state === 'offline'
							? `LAST REPORT ${compactObservedTime}`
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
					<span class="font-mono text-2xs tracking-caps">{canonicalStateLabel}</span>
				</span>
			{/if}
			{#if layoutMode && layoutSize && layoutSelected}
				<span
					class="flex shrink-0 items-center rounded-sm bg-primary px-2.5 py-1.5 font-mono text-2xs font-semibold tracking-caps text-on-primary"
				>
					{layoutSize}
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

	{#if !compactStatus && (presentation.state === 'degraded' || presentation.state === 'stale')}
		<div
			data-peek-camera-region="evidence"
			class="pointer-events-none absolute right-2.5 left-2.5 z-20 rounded-sm border border-activity bg-activity/15 font-medium text-white {mobileFeatured
				? 'bottom-9 px-2.5 py-2 text-xs'
				: 'bottom-2 px-2 py-1.5 text-2xs'} md:bottom-9 md:px-2.5 md:py-2 md:text-xs"
			role="status"
		>
			{#if mobileFeatured}<span>{canonicalStateLabel} —</span>{:else}<span class="hidden md:inline"
					>{canonicalStateLabel} —</span
				>{/if}{' '}{presentation.detail}
		</div>
	{:else if !compactStatus && (presentation.state === 'reconnecting' || presentation.state === 'starting')}
		<div
			class="pointer-events-none absolute inset-0 z-20 grid place-items-center px-3 pt-8 text-center md:px-6 md:pt-0"
		>
			<div data-peek-camera-region="evidence" class="space-y-1 text-white md:space-y-2">
				<RefreshCwIcon
					class="mx-auto size-5 text-text-muted {mobileCompactBlockHiddenClass}"
					strokeWidth={1.75}
				/>
				<p class="text-sm font-medium">{canonicalStateLabel}</p>
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
	{:else if !compactStatus && (presentation.state === 'unknown' || presentation.state === 'stopped')}
		<div
			class="absolute inset-0 z-20 grid place-items-center px-3 pt-8 text-center md:px-6 md:pt-0"
		>
			<div data-peek-camera-region="evidence" class="space-y-1 md:space-y-2">
				<VideoOffIcon class="mx-auto size-5 text-text-muted" strokeWidth={1.75} />
				<p class="text-sm font-semibold text-foreground">{canonicalStateLabel}</p>
				<p class="text-xs text-text-muted">{presentation.detail}</p>
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
