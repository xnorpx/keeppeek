<script lang="ts">
	import { tick } from 'svelte';
	import type { RecordingEvent, RecordingSegment } from '$lib/types';
	import type { TimelineViewport } from '$lib/timeline-repository.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { buildTimelineAvailability } from '$lib/timeline-availability';
	import { TimelinePan } from '$lib/timeline-pan.svelte';
	import ArrowUpIcon from '@lucide/svelte/icons/arrow-up';
	import ScanSearchIcon from '@lucide/svelte/icons/scan-search';
	import ZoomInIcon from '@lucide/svelte/icons/zoom-in';
	import ZoomOutIcon from '@lucide/svelte/icons/zoom-out';

	const DAY_MS = 86_400_000;
	const MINUTE_MS = 60_000;
	const ZOOM_LEVELS = [
		{
			label: '24h',
			pixelsPerHour: 28,
			tickMs: 60 * MINUTE_MS,
			majorEvery: 6,
			bucketMs: 5 * MINUTE_MS,
			prefetchMs: 12 * 60 * MINUTE_MS
		},
		{
			label: '6h',
			pixelsPerHour: 112,
			tickMs: 15 * MINUTE_MS,
			majorEvery: 4,
			bucketMs: 5 * MINUTE_MS,
			prefetchMs: 12 * 60 * MINUTE_MS
		},
		{
			label: '1h',
			pixelsPerHour: 672,
			tickMs: 5 * MINUTE_MS,
			majorEvery: 3,
			bucketMs: MINUTE_MS,
			prefetchMs: 2 * 60 * MINUTE_MS
		},
		{
			label: '15m',
			pixelsPerHour: 2_688,
			tickMs: MINUTE_MS,
			majorEvery: 5,
			bucketMs: 15_000,
			prefetchMs: 30 * MINUTE_MS
		},
		{
			label: '1m',
			pixelsPerHour: 40_320,
			tickMs: 15_000,
			majorEvery: 4,
			bucketMs: 15_000,
			prefetchMs: 30 * MINUTE_MS
		}
	] as const;
	const EVENT_FILTERS = ['all', 'person', 'vehicle', 'motion'] as const;
	const EVENT_CARD_HEIGHT = 68;
	const EVENT_CLUSTER_GAP = 72;
	const timeFormatter = new Intl.DateTimeFormat(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		hour12: false,
		timeZone: 'UTC'
	});

	type EventCluster = {
		event: RecordingEvent;
		count: number;
		top: number;
	};

	type Props = {
		segments: RecordingSegment[];
		events?: RecordingEvent[];
		selectedUrl: string | null;
		playheadMs: number | null;
		dayStartMs: number;
		nowMs?: number;
		followRequest?: number;
		mobileFrame?: boolean;
		paperFrame?: boolean;
		loading?: boolean;
		onSeek: (timestampMs: number) => void;
		onEventPreview?: (event: RecordingEvent) => void;
		onViewportChange?: (viewport: TimelineViewport) => void;
	};

	let {
		segments,
		events = [],
		selectedUrl,
		playheadMs,
		dayStartMs,
		nowMs,
		followRequest = 0,
		mobileFrame = false,
		paperFrame = false,
		loading = false,
		onSeek,
		onEventPreview,
		onViewportChange
	}: Props = $props();

	let zoomIndex = $state(1);
	let eventFilter = $state<(typeof EVENT_FILTERS)[number]>('all');
	let clockNowMs = $state(Date.now());
	let activeDayStartMs = $state<number | null>(null);
	let scroller: HTMLDivElement | null = $state(null);
	let followPlayhead = $state(true);
	let detachedEndMs = $state<number | null>(null);
	let draggedPlayheadMs = $state<number | null>(null);
	let dragPointerId = $state<number | null>(null);
	let viewportTopPx = $state(0);
	let viewportExtentPx = $state(0);
	let dragOffsetY = 0;
	const timelinePan = new TimelinePan();
	let zoomLevel = $derived(ZOOM_LEVELS[zoomIndex]);
	let pixelsPerHour = $derived(paperFrame ? 206.67 : zoomLevel.pixelsPerHour);
	let effectiveNowMs = $derived(nowMs ?? clockNowMs);
	let dayEndMs = $derived(dayStartMs + DAY_MS);
	let liveTimelineEndMs = $derived(
		effectiveNowMs >= dayStartMs && effectiveNowMs < dayEndMs ? effectiveNowMs : dayEndMs
	);
	let timelineEndMs = $derived(detachedEndMs ?? liveTimelineEndMs);
	let timelineStartMs = $derived(dayStartMs);
	let timelineDurationMs = $derived(Math.max(1, timelineEndMs - timelineStartMs));
	let timelineHeight = $derived((pixelsPerHour * timelineDurationMs) / (60 * MINUTE_MS));
	let renderTopPx = $derived(
		viewportExtentPx > 0 ? Math.max(0, viewportTopPx - viewportExtentPx * 2) : 0
	);
	let renderBottomPx = $derived(
		viewportExtentPx > 0
			? Math.min(timelineHeight, viewportTopPx + viewportExtentPx * 3)
			: timelineHeight
	);
	let renderStartMs = $derived(timestampAtTop(renderBottomPx));
	let renderEndMs = $derived(timestampAtTop(renderTopPx));
	let viewportStartMs = $derived(
		timestampAtTop(Math.min(timelineHeight, viewportTopPx + viewportExtentPx))
	);
	let viewportEndMs = $derived(timestampAtTop(Math.max(0, viewportTopPx)));
	let availability = $derived(buildTimelineAvailability(segments, renderStartMs, renderEndMs));
	let isLiveDay = $derived(effectiveNowMs >= dayStartMs && effectiveNowMs < dayEndMs);
	let ticks = $derived.by(() => {
		const values: Array<{ timestampMs: number; major: boolean; top: number }> = [];
		const firstTimestampMs = Math.ceil(renderStartMs / zoomLevel.tickMs) * zoomLevel.tickMs;
		for (
			let timestampMs = firstTimestampMs;
			timestampMs <= renderEndMs;
			timestampMs += zoomLevel.tickMs
		) {
			values.push({
				timestampMs,
				major: Math.floor(timestampMs / zoomLevel.tickMs) % zoomLevel.majorEvery === 0,
				top: timestampTop(timestampMs)
			});
		}
		return values;
	});
	let displayedPlayheadMs = $derived(draggedPlayheadMs ?? playheadMs);
	let playheadTop = $derived(
		displayedPlayheadMs === null ? null : timestampTop(displayedPlayheadMs)
	);
	let visibleEvents = $derived(
		events.filter(
			(event) =>
				event.start_time_ms >= renderStartMs &&
				event.start_time_ms <= renderEndMs &&
				eventMatchesFilter(event.kind)
		)
	);
	let eventClusters = $derived.by(() => {
		const clusters: EventCluster[] = [];
		for (const event of visibleEvents.toSorted(
			(left, right) => right.start_time_ms - left.start_time_ms
		)) {
			const top = timestampTop(event.start_time_ms);
			const previous = clusters.at(-1);
			if (previous && top - previous.top < EVENT_CLUSTER_GAP) {
				previous.count += 1;
				if (!previous.event.thumbnail_url && event.thumbnail_url) previous.event = event;
				continue;
			}
			clusters.push({ event, count: 1, top });
		}
		return clusters;
	});

	function timestampTop(timestampMs: number): number {
		const fraction = Math.max(0, Math.min(1, (timelineEndMs - timestampMs) / timelineDurationMs));
		return fraction * timelineHeight;
	}

	function timestampAtTop(topPx: number): number {
		if (timelineHeight <= 0) return timelineEndMs;
		const fraction = Math.max(0, Math.min(1, topPx / timelineHeight));
		return timelineEndMs - fraction * timelineDurationMs;
	}

	function rangeHeight(startMs: number, endMs: number): number {
		return Math.max(1, timestampTop(startMs) - timestampTop(endMs));
	}

	function eventCardTop(top: number): number {
		return Math.max(
			2,
			Math.min(timelineHeight - EVENT_CARD_HEIGHT - 2, top - EVENT_CARD_HEIGHT / 2)
		);
	}

	function eventLabel(kind: string): string {
		const label = kind.replaceAll(/[-_]/g, ' ').trim();
		return label ? label.charAt(0).toUpperCase() + label.slice(1) : 'Motion';
	}

	function eventMatchesFilter(kind: string): boolean {
		if (eventFilter === 'all') return true;
		const normalizedKind = kind.toLocaleLowerCase().replaceAll(/[-_]/g, ' ');
		return normalizedKind.split(/\s+/).includes(eventFilter);
	}

	function selectEvent(event: RecordingEvent): void {
		onEventPreview?.(event);
		onSeek(event.start_time_ms);
	}

	function filterLabel(filter: (typeof EVENT_FILTERS)[number]): string {
		return filter.charAt(0).toUpperCase() + filter.slice(1);
	}

	function seekFromPointer(event: MouseEvent) {
		stopFollowing();
		const target = event.currentTarget as HTMLButtonElement;
		const rect = target.getBoundingClientRect();
		const fraction = Math.max(0, Math.min(1, (event.clientY - rect.top) / rect.height));
		onSeek(timelineEndMs - fraction * timelineDurationMs);
	}

	function seekFromKeyboard(event: KeyboardEvent) {
		const step = event.shiftKey ? 10 * MINUTE_MS : MINUTE_MS;
		const current = playheadMs ?? timelineEndMs;
		let next: number | null = null;
		if (event.key === 'ArrowUp') next = current + step;
		if (event.key === 'ArrowDown') next = current - step;
		if (event.key === 'Home') next = timelineEndMs;
		if (event.key === 'End') next = timelineStartMs;
		if (next === null) return;
		event.preventDefault();
		stopFollowing();
		onSeek(Math.max(timelineStartMs, Math.min(timelineEndMs, next)));
	}

	function pointerTimelineTop(clientY: number): number | null {
		if (!scroller) return null;
		const rect = scroller.getBoundingClientRect();
		return Math.max(0, Math.min(timelineHeight, scroller.scrollTop + clientY - rect.top));
	}

	function timestampFromDrag(clientY: number): number | null {
		const pointerTop = pointerTimelineTop(clientY);
		if (pointerTop === null) return null;
		const top = Math.max(0, Math.min(timelineHeight, pointerTop - dragOffsetY));
		return timelineEndMs - (top / timelineHeight) * timelineDurationMs;
	}

	function beginPlayheadDrag(event: PointerEvent) {
		if (event.button !== 0 || dragPointerId !== null || playheadTop === null) return;
		const pointerTop = pointerTimelineTop(event.clientY);
		if (pointerTop === null) return;
		event.preventDefault();
		event.stopPropagation();
		dragPointerId = event.pointerId;
		dragOffsetY = pointerTop - playheadTop;
		draggedPlayheadMs = displayedPlayheadMs;
		stopFollowing();
		(event.currentTarget as HTMLButtonElement).setPointerCapture(event.pointerId);
	}

	function movePlayhead(event: PointerEvent) {
		if (event.pointerId !== dragPointerId) return;
		event.preventDefault();
		event.stopPropagation();
		const timestampMs = timestampFromDrag(event.clientY);
		if (timestampMs !== null) draggedPlayheadMs = timestampMs;
	}

	function endPlayheadDrag(event: PointerEvent) {
		if (event.pointerId !== dragPointerId) return;
		event.preventDefault();
		event.stopPropagation();
		const timestampMs = timestampFromDrag(event.clientY) ?? draggedPlayheadMs;
		const target = event.currentTarget as HTMLButtonElement;
		dragPointerId = null;
		dragOffsetY = 0;
		if (target.hasPointerCapture(event.pointerId)) target.releasePointerCapture(event.pointerId);
		if (timestampMs !== null) onSeek(timestampMs);
		draggedPlayheadMs = null;
		stopFollowing();
	}

	function cancelPlayheadDrag(event: PointerEvent) {
		if (event.pointerId !== dragPointerId) return;
		event.stopPropagation();
		dragPointerId = null;
		dragOffsetY = 0;
		draggedPlayheadMs = null;
		stopFollowing();
	}

	function beginPan(event: PointerEvent) {
		const target = scroller ?? (event.currentTarget as HTMLDivElement);
		timelinePan.begin(event, target);
	}

	function endPan(event: PointerEvent) {
		if (timelinePan.end(event)) stopFollowing();
	}

	async function zoom(direction: number) {
		const nextZoomIndex = Math.max(0, Math.min(ZOOM_LEVELS.length - 1, zoomIndex + direction));
		if (nextZoomIndex === zoomIndex) return;
		const node = scroller;
		const markerOffsetPx = node ? node.scrollTop + node.clientHeight / 2 : 0;
		const markerTimestampMs = timestampAtTop(markerOffsetPx);
		zoomIndex = nextZoomIndex;
		await tick();
		if (!node || node.clientHeight <= 0) return;
		node.scrollTop = Math.max(
			0,
			Math.min(
				timelineHeight - node.clientHeight,
				timestampTop(markerTimestampMs) - node.clientHeight / 2
			)
		);
		syncViewport(node);
	}

	function handleWheel(event: WheelEvent): void {
		stopFollowing();
		if (!event.ctrlKey) return;
		event.preventDefault();
		void zoom(event.deltaY > 0 ? -1 : 1);
	}

	function syncViewport(node: HTMLDivElement): void {
		viewportTopPx = node.scrollTop;
		viewportExtentPx = node.clientHeight;
	}

	function handleScroll(event: Event): void {
		syncViewport(event.currentTarget as HTMLDivElement);
	}

	function stopFollowing(): void {
		if (detachedEndMs === null) detachedEndMs = liveTimelineEndMs;
		followPlayhead = false;
	}

	function backToLive(): void {
		detachedEndMs = null;
		followPlayhead = true;
		onSeek(liveTimelineEndMs);
		scroller?.scrollTo({ top: 0, behavior: 'smooth' });
	}

	function formatTime(timestampMs: number): string {
		return timeFormatter.format(new Date(timestampMs));
	}

	$effect(() => {
		if (followRequest <= 0) return;
		backToLive();
	});

	$effect(() => {
		if (nowMs !== undefined) return;
		const timer = window.setInterval(() => {
			clockNowMs = Date.now();
		}, 1_000);
		return () => window.clearInterval(timer);
	});

	$effect(() => {
		const selectedDayStartMs = dayStartMs;
		if (activeDayStartMs === null) {
			activeDayStartMs = selectedDayStartMs;
			return;
		}
		if (activeDayStartMs === selectedDayStartMs) return;
		activeDayStartMs = selectedDayStartMs;
		detachedEndMs = null;
		followPlayhead = true;
	});

	$effect(() => {
		const node = scroller;
		if (!node || !followPlayhead || timelinePan.active) return;
		const frame = requestAnimationFrame(() => {
			node.scrollTo({ top: 0, behavior: 'smooth' });
			syncViewport(node);
		});
		return () => cancelAnimationFrame(frame);
	});

	$effect(() => {
		const node = scroller;
		if (!node) return;
		const update = () => syncViewport(node);
		update();
		const observer = new ResizeObserver(update);
		observer.observe(node);
		return () => observer.disconnect();
	});

	$effect(() => {
		const callback = onViewportChange;
		const extentPx = viewportExtentPx;
		if (!callback || extentPx <= 0) return;
		const viewport: TimelineViewport = {
			startMs: viewportStartMs,
			endMs: viewportEndMs,
			bucketMs: zoomLevel.bucketMs,
			prefetchMs: zoomLevel.prefetchMs,
			viewportExtentPx: extentPx,
			eventTypes: eventFilter === 'all' ? [] : [eventFilter]
		};
		const frame = requestAnimationFrame(() => callback(viewport));
		return () => cancelAnimationFrame(frame);
	});
</script>

<section
	data-timeline-zoom={zoomLevel.label}
	data-timeline-following={followPlayhead}
	data-timeline-end-ms={timelineEndMs}
	aria-busy={loading}
	class="relative flex min-h-0 flex-col overflow-hidden bg-card/95 {paperFrame
		? 'h-[718px] w-[396px] shrink-0 border-l border-hairline [font-synthesis:none]'
		: mobileFrame
			? 'h-[434px] w-full shrink-0 border-y'
			: 'h-[28rem] w-full rounded-md border lg:h-[calc(100svh-10.5rem)] lg:max-h-[52rem] lg:min-h-[32rem] lg:w-[24.75rem]'}"
	aria-label="Recording timeline"
>
	{#if paperFrame}
		<header
			class="flex h-[52px] w-[395px] shrink-0 items-center gap-[26px] border-b border-hairline px-[18px]"
		>
			<span
				class="flex h-[51px] flex-col justify-center font-mono text-[11px] leading-[14px] font-semibold tracking-[0.14em]"
			>
				TIMELINE<span class="absolute top-[50px] h-0.5 w-[66px] bg-primary"></span>
			</span>
			<span
				class="font-mono text-[11px] leading-[14px] font-semibold tracking-[0.14em] text-text-faint"
				>EVENTS</span
			>
			<span
				class="font-mono text-[11px] leading-[14px] font-semibold tracking-[0.14em] text-text-faint"
				>STORIES</span
			>
		</header>
	{:else}
		<header
			class="{mobileFrame
				? 'hidden'
				: 'flex'} h-12 shrink-0 items-center justify-between border-b px-3"
		>
			<div class="flex items-center gap-2">
				<span class="text-xs font-semibold">Timeline</span>
				<span
					class="rounded-full bg-primary/10 px-2 py-0.5 font-mono text-[10px] text-primary-soft"
				>
					{zoomLevel.label}
				</span>
				<span class="font-mono text-[10px] text-muted-foreground tabular-nums">
					{playheadMs === null ? '--:-- UTC' : `${formatTime(playheadMs)} UTC`}
				</span>
			</div>
			<div class="flex items-center">
				<Button
					variant="ghost"
					size="icon-sm"
					title="Zoom timeline out"
					disabled={zoomIndex === 0}
					onclick={() => void zoom(-1)}
				>
					<ZoomOutIcon />
				</Button>
				<Button
					variant="ghost"
					size="icon-sm"
					title="Zoom timeline in"
					disabled={zoomIndex === ZOOM_LEVELS.length - 1}
					onclick={() => void zoom(1)}
				>
					<ZoomInIcon />
				</Button>
			</div>
		</header>
	{/if}

	<div
		class="{mobileFrame && !paperFrame
			? 'hidden'
			: 'flex'} shrink-0 [scrollbar-width:none] items-center gap-1.5 overflow-x-auto border-b border-hairline [&::-webkit-scrollbar]:hidden {paperFrame
			? 'h-[46px] w-[395px] justify-between px-[18px]'
			: 'h-11 px-3'}"
		aria-label="Timeline event filters"
	>
		<div class="flex gap-1.5">
			{#each EVENT_FILTERS as filter (filter)}
				<button
					type="button"
					class="h-6 shrink-0 rounded-full border px-2.5 text-2xs font-medium {eventFilter ===
					filter
						? 'border-primary bg-primary text-on-primary'
						: 'border-hairline-strong text-text-muted hover:text-foreground'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					aria-pressed={eventFilter === filter}
					onclick={() => (eventFilter = filter)}
				>
					{filterLabel(filter)}
				</button>
			{/each}
		</div>
		{#if paperFrame}
			<div class="flex items-center gap-2.5">
				<button
					type="button"
					class="grid size-4 place-items-center text-text-muted"
					title="Zoom timeline in"
					onclick={() => void zoom(1)}><ZoomInIcon class="size-3.5" /></button
				>
				<button
					type="button"
					class="grid size-4 place-items-center text-text-muted"
					title="Zoom timeline out"
					onclick={() => void zoom(-1)}><ZoomOutIcon class="size-3.5" /></button
				>
			</div>
		{/if}
	</div>

	{#if !followPlayhead}
		<button
			type="button"
			class="absolute top-[6.5rem] left-1/2 z-50 inline-flex h-8 -translate-x-1/2 items-center gap-1.5 rounded-full bg-primary px-3 text-xs font-semibold text-on-primary shadow-md focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			onclick={backToLive}
		>
			<ArrowUpIcon class="size-3.5" />
			Back to live
		</button>
	{/if}

	<div
		bind:this={scroller}
		class="min-h-0 touch-none [scrollbar-width:none] overflow-y-auto overscroll-contain bg-muted/15 [&::-webkit-scrollbar]:hidden {timelinePan.cursorClass} {paperFrame
			? 'h-[620px] w-[395px] shrink-0'
			: 'flex-1'}"
		role="region"
		aria-label="Recording timeline pan viewport"
		onpointerdown={beginPan}
		onpointermove={(event) => timelinePan.move(event)}
		onpointerup={endPan}
		onpointercancel={(event) => timelinePan.cancel(event)}
		onlostpointercapture={(event) => timelinePan.cancel(event)}
		onclickcapture={(event) => timelinePan.consumeClick(event)}
		onwheel={handleWheel}
		onscroll={handleScroll}
	>
		<div class="relative min-w-full" style={`height: ${timelineHeight}px`}>
			<button
				type="button"
				class="absolute inset-x-0 top-0 z-0 {timelinePan.cursorClass} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
				style={`height: ${timelineHeight}px`}
				aria-label="Seek recording timeline. Use arrow keys to move one minute and Shift plus arrow keys to move ten minutes."
				onclick={seekFromPointer}
				onkeydown={seekFromKeyboard}
			></button>

			{#each ticks as tick (tick.timestampMs)}
				<div
					data-timeline-tick
					class="pointer-events-none absolute left-0 z-10 flex w-14 items-center"
					style={`top: ${tick.top}px`}
				>
					{#if tick.major}
						<span
							class="w-12 -translate-y-1/2 pr-1.5 text-right font-mono text-[10px] text-muted-foreground"
						>
							{formatTime(tick.timestampMs)}
						</span>
					{/if}
					<span class="h-px flex-1 {tick.major ? 'bg-border' : 'ml-12 bg-border/40'}"></span>
				</div>
			{/each}

			<div class="absolute top-0 bottom-0 left-14 z-20 w-3.5">
				{#each availability.available as range (`${range.startMs}-${range.endMs}`)}
					<button
						type="button"
						data-timeline-availability
						data-start-ms={range.startMs}
						data-end-ms={range.endMs}
						class="absolute inset-x-0 min-h-px appearance-none bg-availability transition-colors hover:bg-primary-soft focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {selectedUrl !==
							null && range.segmentUrls.includes(selectedUrl)
							? 'ring-1 ring-primary'
							: ''}"
						style:top={`${timestampTop(range.endMs)}px`}
						style:height={`${rangeHeight(range.startMs, range.endMs)}px`}
						title={`Footage ${formatTime(range.startMs)}–${formatTime(range.endMs)}`}
						onclick={() => onSeek((range.startMs + range.endMs) / 2)}
					></button>
				{/each}
				{#each availability.gaps as gap (`${gap.startMs}-${gap.endMs}`)}
					<div
						data-timeline-gap
						data-start-ms={gap.startMs}
						data-end-ms={gap.endMs}
						class="absolute inset-x-0 border-y border-dashed border-hairline-strong bg-ground"
						style:top={`${timestampTop(gap.endMs)}px`}
						style:height={`${rangeHeight(gap.startMs, gap.endMs)}px`}
						title={`No footage ${formatTime(gap.startMs)}–${formatTime(gap.endMs)}`}
					></div>
				{/each}
				{#each visibleEvents as event (event.id)}
					{#if event.end_time_ms !== null && event.end_time_ms > event.start_time_ms}
						<span
							data-timeline-activity={event.id}
							class="pointer-events-none absolute inset-x-0 z-20 min-h-0.5 bg-activity"
							style:top={`${timestampTop(event.end_time_ms)}px`}
							style:height={`${Math.max(2, rangeHeight(event.start_time_ms, event.end_time_ms))}px`}
						></span>
					{/if}
					<span
						data-timeline-event-marker={event.id}
						class="pointer-events-none absolute left-1/2 z-30 -translate-x-1/2 rounded-full bg-primary {displayedPlayheadMs !==
							null &&
						displayedPlayheadMs >= event.start_time_ms &&
						displayedPlayheadMs <= (event.end_time_ms ?? event.start_time_ms)
							? 'size-3 border-2 border-foreground'
							: 'size-2'}"
						style:top={`${timestampTop(event.start_time_ms) - (displayedPlayheadMs !== null && displayedPlayheadMs >= event.start_time_ms && displayedPlayheadMs <= (event.end_time_ms ?? event.start_time_ms) ? 6 : 4)}px`}
					></span>
				{/each}
			</div>

			{#each eventClusters as cluster (cluster.event.id)}
				<span
					class="pointer-events-none absolute left-[4.375rem] z-20 h-px w-4 bg-hairline-strong"
					style:top={`${cluster.top}px`}
				></span>
				<button
					type="button"
					data-timeline-event={cluster.event.id}
					class="absolute left-[5.375rem] z-30 flex h-[68px] overflow-hidden rounded-sm border bg-surface p-1.5 text-left shadow-sm transition-colors hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {paperFrame
						? 'right-[9px]'
						: 'right-2'} {playheadMs !== null &&
					playheadMs >= cluster.event.start_time_ms &&
					playheadMs <= (cluster.event.end_time_ms ?? cluster.event.start_time_ms)
						? 'border-primary'
						: 'border-hairline'}"
					style={`top: ${eventCardTop(cluster.top)}px`}
					aria-label={`${eventLabel(cluster.event.kind)} event at ${formatTime(cluster.event.start_time_ms)}`}
					title={`${eventLabel(cluster.event.kind)} · ${formatTime(cluster.event.start_time_ms)}`}
					onclick={() => selectEvent(cluster.event)}
				>
					{#if cluster.event.thumbnail_url}
						{#if paperFrame}
							<span class="h-[53px] w-[94px] shrink-0 rounded-[2px] bg-video"></span>
						{:else}
							<img
								src={cluster.event.thumbnail_url}
								alt=""
								loading="lazy"
								decoding="async"
								class="h-[53px] w-[94px] shrink-0 rounded-[2px] object-cover"
							/>
						{/if}
					{:else}
						<span
							class="grid h-[53px] w-[94px] shrink-0 place-items-center rounded-[2px] border border-dashed border-hairline-strong text-text-faint"
						>
							<ScanSearchIcon class="size-5" />
						</span>
					{/if}
					<span class="min-w-0 flex-1 self-center">
						<span class="block font-mono text-2xs text-text-muted">
							{formatTime(cluster.event.start_time_ms)}
						</span>
						<span class="block truncate text-xs font-medium text-foreground">
							{eventLabel(cluster.event.kind)}
						</span>
						<span class="block truncate text-2xs text-text-faint">
							{cluster.event.source}{cluster.event.confidence === null
								? ''
								: ` · ${cluster.event.confidence.toFixed(2)}`}
						</span>
					</span>
					{#if cluster.count > 1}
						<span
							class="absolute top-1 right-1 rounded-sm bg-black/75 px-1 py-0.5 text-[9px] font-semibold text-white"
						>
							+{cluster.count - 1}
						</span>
					{/if}
				</button>
			{/each}

			{#if isLiveDay}
				<div class="pointer-events-none absolute inset-x-0 top-0 z-40 flex h-3.5 items-center">
					<span
						class="grid h-3.5 w-12 place-items-center rounded-[2px] bg-live font-mono text-2xs font-semibold tracking-caps text-white"
					>
						LIVE
					</span>
					<span class="h-px flex-1 bg-live"></span>
				</div>
			{/if}

			{#if playheadTop !== null && displayedPlayheadMs !== null}
				<button
					type="button"
					class="absolute left-0 z-40 flex h-7 w-[5.4375rem] -translate-y-1/2 touch-none items-center select-none focus-visible:ring-2 focus-visible:ring-red-500 focus-visible:outline-none {dragPointerId ===
					null
						? 'cursor-grab'
						: 'cursor-grabbing'}"
					style={`top: ${playheadTop}px`}
					aria-label={`Playback position at ${formatTime(displayedPlayheadMs)} UTC. Drag vertically to seek.`}
					title="Drag playback position"
					onpointerdown={beginPlayheadDrag}
					onpointermove={movePlayhead}
					onpointerup={endPlayheadDrag}
					onpointercancel={cancelPlayheadDrag}
					onlostpointercapture={cancelPlayheadDrag}
					onkeydown={seekFromKeyboard}
				>
					<span
						class="w-11 rounded-sm bg-red-500 px-1 py-0.5 text-center font-mono text-[9px] font-semibold text-white"
					>
						{formatTime(displayedPlayheadMs)}
					</span>
					<span class="h-0.5 flex-1 bg-red-500 shadow-[0_0_5px_rgba(239,68,68,0.7)]"></span>
				</button>
			{/if}
		</div>
	</div>
</section>
