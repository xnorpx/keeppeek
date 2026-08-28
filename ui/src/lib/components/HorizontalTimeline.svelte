<script lang="ts">
	import { tick } from 'svelte';
	import type { RecordingEvent, RecordingSegment } from '$lib/types';
	import type { TimelineViewport } from '$lib/timeline-repository.svelte';
	import { buildTimelineAvailability } from '$lib/timeline-availability';
	import ZoomInIcon from '@lucide/svelte/icons/zoom-in';
	import ZoomOutIcon from '@lucide/svelte/icons/zoom-out';

	const DAY_MS = 86_400_000;
	const MINUTE_MS = 60_000;
	const ZOOM_LEVELS = [
		{
			label: 'Coarse',
			tickMs: 5 * MINUTE_MS,
			bucketMs: 5 * MINUTE_MS,
			prefetchMs: 60 * MINUTE_MS
		},
		{ label: 'Normal', tickMs: MINUTE_MS, bucketMs: MINUTE_MS, prefetchMs: 30 * MINUTE_MS },
		{ label: 'Fine', tickMs: 15_000, bucketMs: 15_000, prefetchMs: 10 * MINUTE_MS }
	] as const;
	const TICK_EXTENT_PX = 12;
	const EVENT_CLUSTER_GAP_PX = 96;
	const timeFormatter = new Intl.DateTimeFormat(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		hour12: false,
		timeZone: 'UTC'
	});

	type Props = {
		segments: RecordingSegment[];
		events?: RecordingEvent[];
		selectedUrl: string | null;
		playheadMs: number | null;
		dayStartMs: number;
		nowMs?: number;
		followRequest?: number;
		loading?: boolean;
		onSeek: (timestampMs: number) => void;
		onEventPreview?: (event: RecordingEvent) => void;
		onScrubStart?: (timestampMs: number) => void;
		onScrub?: (timestampMs: number) => void;
		onScrubEnd?: (timestampMs: number) => void;
		onScrubCancel?: () => void;
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
		loading = false,
		onSeek,
		onEventPreview,
		onScrubStart,
		onScrub,
		onScrubEnd,
		onScrubCancel,
		onViewportChange
	}: Props = $props();

	let zoomIndex = $state(0);
	let clockNowMs = $state(Date.now());
	let scroller: HTMLDivElement | null = $state(null);
	let viewportLeftPx = $state(0);
	let viewportExtentPx = $state(0);
	let dragPointerId: number | null = null;
	let dragStartX = 0;
	let dragStartScrollLeft = 0;
	let suppressClick = false;
	let centeredDayStartMs: number | null = null;
	let zoomLevel = $derived(ZOOM_LEVELS[zoomIndex]);
	let effectiveNowMs = $derived(nowMs ?? clockNowMs);
	let dayEndMs = $derived(dayStartMs + DAY_MS);
	let timelineEndMs = $derived(
		effectiveNowMs >= dayStartMs && effectiveNowMs < dayEndMs ? effectiveNowMs : dayEndMs
	);
	let timelineDurationMs = $derived(Math.max(1, timelineEndMs - dayStartMs));
	let timelineWidth = $derived(
		Math.max(viewportExtentPx, (timelineDurationMs / zoomLevel.tickMs) * TICK_EXTENT_PX)
	);
	let renderLeftPx = $derived(Math.max(0, viewportLeftPx - viewportExtentPx));
	let renderRightPx = $derived(Math.min(timelineWidth, viewportLeftPx + viewportExtentPx * 2));
	let renderStartMs = $derived(timestampAtLeft(renderLeftPx));
	let renderEndMs = $derived(timestampAtLeft(renderRightPx));
	let viewportStartMs = $derived(timestampAtLeft(viewportLeftPx));
	let viewportEndMs = $derived(timestampAtLeft(viewportLeftPx + viewportExtentPx));
	let markerTimestampMs = $derived(
		timestampAtLeft(viewportLeftPx + Math.max(0, viewportExtentPx / 2))
	);
	let availability = $derived(
		buildTimelineAvailability(segments, renderStartMs, Math.max(renderStartMs + 1, renderEndMs))
	);
	let ticks = $derived.by(() => {
		const first = Math.ceil(renderStartMs / zoomLevel.tickMs) * zoomLevel.tickMs;
		const values: Array<{ timestampMs: number; left: number; major: boolean }> = [];
		for (let timestampMs = first; timestampMs <= renderEndMs; timestampMs += zoomLevel.tickMs) {
			values.push({
				timestampMs,
				left: timestampLeft(timestampMs),
				major: Math.floor(timestampMs / zoomLevel.tickMs) % 4 === 0
			});
		}
		return values;
	});
	let visibleEvents = $derived(
		events.filter(
			(event) => event.start_time_ms <= renderEndMs && eventRangeEnd(event) >= renderStartMs
		)
	);
	let operationalIntervals = $derived(visibleEvents.filter((event) => event.operational));
	let eventClusters = $derived.by(() => {
		const clusters: Array<{ event: RecordingEvent; count: number; left: number }> = [];
		for (const event of visibleEvents.toSorted(
			(left, right) => left.start_time_ms - right.start_time_ms
		)) {
			const left = timestampLeft(event.start_time_ms);
			const previous = clusters.at(-1);
			if (previous && left - previous.left < EVENT_CLUSTER_GAP_PX) {
				previous.count += 1;
				if (!previous.event.thumbnail_url && event.thumbnail_url) previous.event = event;
				continue;
			}
			clusters.push({ event, count: 1, left });
		}
		return clusters;
	});

	function timestampLeft(timestampMs: number): number {
		return ((timestampMs - dayStartMs) / timelineDurationMs) * timelineWidth;
	}

	function timestampAtLeft(leftPx: number): number {
		return (
			dayStartMs +
			(Math.max(0, Math.min(timelineWidth, leftPx)) / timelineWidth) * timelineDurationMs
		);
	}

	function rangeWidth(startMs: number, endMs: number): number {
		return Math.max(1, timestampLeft(endMs) - timestampLeft(startMs));
	}

	function eventRangeEnd(event: RecordingEvent): number {
		return event.end_time_ms ?? (event.operational ? timelineEndMs : event.start_time_ms);
	}

	function eventCardLeft(leftPx: number): number {
		const halfCardWidth = 44;
		return Math.max(
			viewportLeftPx + halfCardWidth,
			Math.min(viewportLeftPx + viewportExtentPx - halfCardWidth, leftPx)
		);
	}

	function syncViewport(node: HTMLDivElement): void {
		viewportLeftPx = node.scrollLeft;
		viewportExtentPx = node.clientWidth;
	}

	function centerTimestamp(timestampMs: number, behavior: ScrollBehavior = 'auto'): void {
		const node = scroller;
		if (!node) return;
		node.scrollTo({
			left: Math.max(0, timestampLeft(timestampMs) - node.clientWidth / 2),
			behavior
		});
		syncViewport(node);
	}

	function beginDrag(event: PointerEvent): void {
		if (event.button !== 0 || dragPointerId !== null || !scroller) return;
		dragPointerId = event.pointerId;
		dragStartX = event.clientX;
		dragStartScrollLeft = scroller.scrollLeft;
		suppressClick = false;
		(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
		onScrubStart?.(markerTimestampMs);
	}

	function moveDrag(event: PointerEvent): void {
		if (event.pointerId !== dragPointerId || !scroller) return;
		const delta = event.clientX - dragStartX;
		if (Math.abs(delta) > 3) suppressClick = true;
		scroller.scrollLeft = dragStartScrollLeft - delta;
		syncViewport(scroller);
		onScrub?.(markerTimestampMs);
	}

	function endDrag(event: PointerEvent): void {
		if (event.pointerId !== dragPointerId) return;
		dragPointerId = null;
		const target = event.currentTarget as HTMLElement;
		if (target.hasPointerCapture(event.pointerId)) target.releasePointerCapture(event.pointerId);
		if (onScrubEnd) onScrubEnd(markerTimestampMs);
		else onSeek(markerTimestampMs);
	}

	function cancelDrag(event: PointerEvent): void {
		if (event.pointerId !== dragPointerId) return;
		dragPointerId = null;
		onScrubCancel?.();
	}

	function seekFromPointer(event: MouseEvent): void {
		if (suppressClick) {
			suppressClick = false;
			return;
		}
		const target = event.currentTarget as HTMLElement;
		const rect = target.getBoundingClientRect();
		onSeek(timestampAtLeft(viewportLeftPx + event.clientX - rect.left));
	}

	function seekFromKeyboard(event: KeyboardEvent): void {
		const step = event.shiftKey ? 10 * MINUTE_MS : MINUTE_MS;
		let target = markerTimestampMs;
		if (event.key === 'ArrowLeft') target -= step;
		else if (event.key === 'ArrowRight') target += step;
		else if (event.key === 'Home') target = dayStartMs;
		else if (event.key === 'End') target = timelineEndMs;
		else return;
		event.preventDefault();
		target = Math.max(dayStartMs, Math.min(timelineEndMs, target));
		centerTimestamp(target);
		onSeek(target);
	}

	async function zoom(direction: number): Promise<void> {
		const next = Math.max(0, Math.min(ZOOM_LEVELS.length - 1, zoomIndex + direction));
		if (next === zoomIndex) return;
		const selectedMs = markerTimestampMs;
		zoomIndex = next;
		await tick();
		centerTimestamp(selectedMs);
	}

	function formatTime(timestampMs: number): string {
		return timeFormatter.format(new Date(timestampMs));
	}

	function eventLabel(kind: string): string {
		const label = kind.replaceAll(/[-_]/g, ' ').trim();
		return label ? label.charAt(0).toUpperCase() + label.slice(1) : 'Motion';
	}

	$effect(() => {
		if (nowMs !== undefined) return;
		const timer = window.setInterval(() => (clockNowMs = Date.now()), 1_000);
		return () => window.clearInterval(timer);
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
		const selectedMs = playheadMs;
		const selectedDayStartMs = dayStartMs;
		if (
			!scroller ||
			selectedMs === null ||
			viewportExtentPx <= 0 ||
			centeredDayStartMs === selectedDayStartMs
		) {
			return;
		}
		centeredDayStartMs = selectedDayStartMs;
		const frame = requestAnimationFrame(() => centerTimestamp(selectedMs));
		return () => cancelAnimationFrame(frame);
	});

	$effect(() => {
		if (followRequest <= 0) return;
		requestAnimationFrame(() => centerTimestamp(playheadMs ?? timelineEndMs, 'smooth'));
	});

	$effect(() => {
		const callback = onViewportChange;
		if (!callback || viewportExtentPx <= 0) return;
		const viewport: TimelineViewport = {
			startMs: viewportStartMs,
			endMs: viewportEndMs,
			bucketMs: zoomLevel.bucketMs,
			prefetchMs: zoomLevel.prefetchMs,
			viewportExtentPx,
			eventTypes: []
		};
		const frame = requestAnimationFrame(() => callback(viewport));
		return () => cancelAnimationFrame(frame);
	});
</script>

<section
	class="relative h-40 overflow-hidden border-y bg-card/95"
	aria-label="Recording timeline"
	aria-busy={loading}
	data-timeline-orientation="horizontal"
	data-timeline-zoom={zoomLevel.label}
>
	<header class="flex h-10 items-center justify-between border-b px-3">
		<div class="flex min-w-0 items-center gap-2">
			<span class="text-xs font-semibold">Timeline</span>
			<span class="font-mono text-[10px] text-muted-foreground tabular-nums">
				{formatTime(markerTimestampMs)} UTC
			</span>
		</div>
		<div class="flex items-center gap-1">
			<button
				type="button"
				class="grid size-11 place-items-center rounded-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40 md:size-7"
				title="Zoom timeline out"
				disabled={zoomIndex === 0}
				onclick={() => void zoom(-1)}><ZoomOutIcon class="size-3.5" /></button
			>
			<button
				type="button"
				class="grid size-11 place-items-center rounded-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40 md:size-7"
				title="Zoom timeline in"
				disabled={zoomIndex === ZOOM_LEVELS.length - 1}
				onclick={() => void zoom(1)}><ZoomInIcon class="size-3.5" /></button
			>
		</div>
	</header>

	<div
		bind:this={scroller}
		class="h-[7.5rem] touch-none [scrollbar-width:none] overflow-x-auto overflow-y-hidden overscroll-x-contain [&::-webkit-scrollbar]:hidden"
		role="slider"
		tabindex="0"
		aria-label="Recording timeline scrubber"
		aria-valuemin={dayStartMs}
		aria-valuemax={timelineEndMs}
		aria-valuenow={Math.round(markerTimestampMs)}
		aria-valuetext={`${formatTime(markerTimestampMs)} UTC`}
		onscroll={(event) => syncViewport(event.currentTarget)}
		onpointerdown={beginDrag}
		onpointermove={moveDrag}
		onpointerup={endDrag}
		onpointercancel={cancelDrag}
		onkeydown={seekFromKeyboard}
	>
		<div
			class="relative h-full"
			style:width={`${timelineWidth}px`}
			onclick={seekFromPointer}
			role="presentation"
		>
			{#each ticks as tick (tick.timestampMs)}
				<div
					class="pointer-events-none absolute top-0 h-8 border-l {tick.major
						? 'border-border'
						: 'border-border/40'}"
					style:left={`${tick.left}px`}
				>
					{#if tick.major}
						<span class="ml-1 font-mono text-[9px] text-muted-foreground">
							{formatTime(tick.timestampMs)}
						</span>
					{/if}
				</div>
			{/each}
			<div class="absolute top-9 right-0 left-0 h-3">
				{#each availability.gaps as gap (`${gap.startMs}-${gap.endMs}`)}
					<div
						data-timeline-gap
						data-start-ms={gap.startMs}
						data-end-ms={gap.endMs}
						class="absolute inset-y-0 border-x border-dashed border-hairline-strong bg-ground"
						style:left={`${timestampLeft(gap.startMs)}px`}
						style:width={`${rangeWidth(gap.startMs, gap.endMs)}px`}
					></div>
				{/each}
				{#each availability.available as range (`${range.startMs}-${range.endMs}`)}
					<button
						type="button"
						class="absolute inset-y-0 bg-availability hover:bg-primary-soft focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {selectedUrl &&
						range.segmentUrls.includes(selectedUrl)
							? 'ring-1 ring-primary'
							: ''}"
						style:left={`${timestampLeft(range.startMs)}px`}
						style:width={`${rangeWidth(range.startMs, range.endMs)}px`}
						title={`Footage ${formatTime(range.startMs)}–${formatTime(range.endMs)}`}
						onclick={(event) => {
							event.stopPropagation();
							onSeek((range.startMs + range.endMs) / 2);
						}}
					></button>
				{/each}
				{#each operationalIntervals as event (event.id)}
					<button
						type="button"
						data-timeline-operational-event={event.id}
						class="absolute inset-y-0 z-20 min-w-px border-x {event.operational?.severity ===
						'critical'
							? 'border-live bg-live/70'
							: 'border-activity bg-activity/70'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						style:left={`${timestampLeft(Math.max(event.start_time_ms, renderStartMs))}px`}
						style:width={`${rangeWidth(Math.max(event.start_time_ms, renderStartMs), Math.min(eventRangeEnd(event), renderEndMs))}px`}
						title={`${eventLabel(event.kind)} ${formatTime(event.start_time_ms)}–${event.end_time_ms === null ? 'ongoing' : formatTime(event.end_time_ms)}`}
						onclick={(pointerEvent) => {
							pointerEvent.stopPropagation();
							onEventPreview?.(event);
							onSeek(event.start_time_ms);
						}}
					></button>
				{/each}
			</div>
			{#each eventClusters as cluster (cluster.event.id)}
				<button
					type="button"
					class="absolute top-14 flex h-11 w-[88px] -translate-x-1/2 items-center gap-1 overflow-hidden rounded-sm border border-hairline bg-surface p-1 text-left shadow-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					style:left={`${eventCardLeft(cluster.left)}px`}
					aria-label={`${eventLabel(cluster.event.kind)} event at ${formatTime(cluster.event.start_time_ms)}`}
					onclick={(pointerEvent) => {
						pointerEvent.stopPropagation();
						onEventPreview?.(cluster.event);
						onSeek(cluster.event.start_time_ms);
					}}
				>
					{#if cluster.event.thumbnail_url}
						<img
							src={cluster.event.thumbnail_url}
							alt=""
							loading="lazy"
							decoding="async"
							class="h-9 w-12 shrink-0 rounded-[2px] object-cover"
						/>
					{/if}
					<span class="min-w-0 flex-1">
						<span class="block truncate text-[9px] font-medium">
							{eventLabel(cluster.event.kind)}
						</span>
						<span class="block font-mono text-[8px] text-text-faint">
							{formatTime(cluster.event.start_time_ms)}
						</span>
					</span>
					{#if cluster.count > 1}
						<span class="absolute top-0 right-0 bg-black/75 px-1 text-[8px] text-white">
							+{cluster.count - 1}
						</span>
					{/if}
				</button>
			{/each}
		</div>
	</div>
	<div
		class="pointer-events-none absolute top-10 bottom-0 left-1/2 z-30 w-0.5 -translate-x-1/2 bg-red-500"
	>
		<span
			class="absolute top-1 left-1/2 -translate-x-1/2 rounded-sm bg-red-500 px-1 py-0.5 font-mono text-[9px] font-semibold text-white"
		>
			{formatTime(markerTimestampMs)}
		</span>
	</div>
</section>
