<script lang="ts">
	import type { RecordingEvent, RecordingSegment } from '$lib/types';
	import { Button } from '$lib/components/ui/button/index.js';
	import { TimelinePan } from '$lib/timeline-pan.svelte';
	import ScanSearchIcon from '@lucide/svelte/icons/scan-search';
	import ZoomInIcon from '@lucide/svelte/icons/zoom-in';
	import ZoomOutIcon from '@lucide/svelte/icons/zoom-out';

	const DAY_MS = 86_400_000;
	const MINUTE_MS = 60_000;
	const ZOOM_LEVELS = [48, 72, 96, 144];
	const EVENT_CARD_HEIGHT = 56;
	const EVENT_CLUSTER_GAP = 60;
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
		onSeek: (timestampMs: number) => void;
	};

	let { segments, events = [], selectedUrl, playheadMs, dayStartMs, onSeek }: Props = $props();

	let pixelsPerHour = $state(72);
	let scroller: HTMLDivElement | null = $state(null);
	let followPlayhead = $state(true);
	let draggedPlayheadMs = $state<number | null>(null);
	let dragPointerId = $state<number | null>(null);
	let dragOffsetY = 0;
	const timelinePan = new TimelinePan();
	let timelineHeight = $derived(pixelsPerHour * 24);
	let ticks = $derived(
		Array.from({ length: 97 }, (_, index) => ({
			index,
			major: index % 4 === 0,
			top: (index / 96) * timelineHeight
		}))
	);
	let displayedPlayheadMs = $derived(draggedPlayheadMs ?? playheadMs);
	let playheadTop = $derived(
		displayedPlayheadMs === null ? null : timestampTop(displayedPlayheadMs)
	);
	let eventClusters = $derived.by(() => {
		const clusters: EventCluster[] = [];
		for (const event of events.toSorted(
			(left, right) => left.start_time_ms - right.start_time_ms
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
		const fraction = Math.max(0, Math.min(1, (timestampMs - dayStartMs) / DAY_MS));
		return fraction * timelineHeight;
	}

	function segmentHeight(segment: RecordingSegment): number {
		const availableHeight = timestampTop(segment.end_time_ms) - timestampTop(segment.start_time_ms);
		return Math.max(Math.min(0.5, availableHeight), availableHeight - 1);
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

	function seekFromPointer(event: MouseEvent) {
		followPlayhead = true;
		const target = event.currentTarget as HTMLButtonElement;
		const rect = target.getBoundingClientRect();
		const fraction = Math.max(0, Math.min(1, (event.clientY - rect.top) / rect.height));
		onSeek(dayStartMs + fraction * DAY_MS);
	}

	function seekFromKeyboard(event: KeyboardEvent) {
		const step = event.shiftKey ? 10 * MINUTE_MS : MINUTE_MS;
		const current = playheadMs ?? dayStartMs;
		let next: number | null = null;
		if (event.key === 'ArrowUp') next = current - step;
		if (event.key === 'ArrowDown') next = current + step;
		if (event.key === 'Home') next = dayStartMs;
		if (event.key === 'End') next = dayStartMs + DAY_MS - 1;
		if (next === null) return;
		event.preventDefault();
		followPlayhead = true;
		onSeek(Math.max(dayStartMs, Math.min(dayStartMs + DAY_MS - 1, next)));
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
		return dayStartMs + (top / timelineHeight) * DAY_MS;
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
		followPlayhead = false;
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
		followPlayhead = true;
	}

	function cancelPlayheadDrag(event: PointerEvent) {
		if (event.pointerId !== dragPointerId) return;
		event.stopPropagation();
		dragPointerId = null;
		dragOffsetY = 0;
		draggedPlayheadMs = null;
		followPlayhead = true;
	}

	function beginPan(event: PointerEvent) {
		const target = scroller ?? (event.currentTarget as HTMLDivElement);
		timelinePan.begin(event, target);
	}

	function endPan(event: PointerEvent) {
		if (timelinePan.end(event)) followPlayhead = false;
	}

	function zoom(direction: number) {
		const index = ZOOM_LEVELS.indexOf(pixelsPerHour);
		const next = Math.max(0, Math.min(ZOOM_LEVELS.length - 1, index + direction));
		pixelsPerHour = ZOOM_LEVELS[next];
	}

	function formatTime(timestampMs: number): string {
		return timeFormatter.format(new Date(timestampMs));
	}

	$effect(() => {
		const node = scroller;
		const top = playheadTop;
		if (!node || top === null || !followPlayhead || timelinePan.active) return;
		const margin = 96;
		if (top < node.scrollTop + margin || top > node.scrollTop + node.clientHeight - margin) {
			requestAnimationFrame(() => {
				node.scrollTo({
					top: Math.max(0, top - node.clientHeight / 2),
					behavior: 'smooth'
				});
			});
		}
	});
</script>

<section
	class="flex h-[28rem] min-h-0 flex-col overflow-hidden rounded-md border bg-card/95 lg:h-[calc(100svh-10.5rem)] lg:max-h-[52rem] lg:min-h-[32rem]"
	aria-label="Recording timeline"
>
	<header class="flex h-12 shrink-0 items-center justify-between border-b px-3">
		<div class="flex items-center gap-2">
			<span class="text-xs font-semibold">Timeline</span>
			<span class="font-mono text-[10px] text-muted-foreground tabular-nums">
				{playheadMs === null ? '--:-- UTC' : `${formatTime(playheadMs)} UTC`}
			</span>
		</div>
		<div class="flex items-center">
			<Button
				variant="ghost"
				size="icon-sm"
				title="Zoom timeline out"
				disabled={pixelsPerHour === ZOOM_LEVELS[0]}
				onclick={() => zoom(-1)}
			>
				<ZoomOutIcon />
			</Button>
			<Button
				variant="ghost"
				size="icon-sm"
				title="Zoom timeline in"
				disabled={pixelsPerHour === ZOOM_LEVELS.at(-1)}
				onclick={() => zoom(1)}
			>
				<ZoomInIcon />
			</Button>
		</div>
	</header>

	<div
		bind:this={scroller}
		class="min-h-0 flex-1 touch-none overflow-y-auto overscroll-contain bg-muted/15 {timelinePan.cursorClass}"
		role="region"
		aria-label="Recording timeline pan viewport"
		onpointerdown={beginPan}
		onpointermove={(event) => timelinePan.move(event)}
		onpointerup={endPan}
		onpointercancel={(event) => timelinePan.cancel(event)}
		onlostpointercapture={(event) => timelinePan.cancel(event)}
		onclickcapture={(event) => timelinePan.consumeClick(event)}
	>
		<div class="relative min-w-[9rem]" style={`height: ${timelineHeight}px`}>
			<button
				type="button"
				class="absolute inset-x-0 top-0 z-0 {timelinePan.cursorClass} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
				style={`height: ${timelineHeight}px`}
				aria-label="Seek recording timeline. Use arrow keys to move one minute and Shift plus arrow keys to move ten minutes."
				onclick={seekFromPointer}
				onkeydown={seekFromKeyboard}
			></button>

			{#each ticks as tick (tick.index)}
				<div
					class="pointer-events-none absolute right-0 left-0 z-10 flex items-center"
					style={`top: ${tick.top}px`}
				>
					{#if tick.major}
						<span
							class="w-11 -translate-y-1/2 pr-2 text-right font-mono text-[10px] text-muted-foreground"
						>
							{String(tick.index / 4).padStart(2, '0')}:00
						</span>
					{/if}
					<span class="h-px flex-1 {tick.major ? 'bg-border' : 'ml-11 bg-border/40'}"></span>
				</div>
			{/each}

			<div class="absolute top-0 right-2 bottom-0 left-12 z-20">
				{#each segments as segment (segment.url)}
					<button
						type="button"
						class="absolute right-0 left-0 min-h-0 appearance-none overflow-hidden rounded-sm border-l-2 px-1 py-0 text-left text-[10px] transition-colors focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {selectedUrl ===
						segment.url
							? 'border-blue-300 bg-blue-600 text-white'
							: 'border-sky-500 bg-sky-400/40 hover:bg-sky-400/60'}"
						style={`top: ${timestampTop(segment.start_time_ms)}px; height: ${segmentHeight(segment)}px`}
						title={`${formatTime(segment.start_time_ms)}–${formatTime(segment.end_time_ms)}`}
						onclick={() => onSeek(segment.start_time_ms)}
					>
						{#if segmentHeight(segment) >= 20}
							{formatTime(segment.start_time_ms)}
						{/if}
					</button>
				{/each}
			</div>

			{#each eventClusters as cluster (cluster.event.id)}
				<button
					type="button"
					class="absolute right-2 z-30 h-14 w-24 overflow-hidden rounded border border-white/80 bg-zinc-900 text-left shadow-md ring-1 ring-black/15 transition-transform hover:scale-[1.03] focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					style={`top: ${eventCardTop(cluster.top)}px`}
					aria-label={`${eventLabel(cluster.event.kind)} event at ${formatTime(cluster.event.start_time_ms)}`}
					title={`${eventLabel(cluster.event.kind)} · ${formatTime(cluster.event.start_time_ms)}`}
					onclick={() => onSeek(cluster.event.start_time_ms)}
				>
					{#if cluster.event.thumbnail_url}
						<img
							src={cluster.event.thumbnail_url}
							alt=""
							loading="lazy"
							decoding="async"
							class="size-full object-cover"
						/>
					{:else}
						<span class="grid size-full place-items-center text-white/55">
							<ScanSearchIcon class="size-5" />
						</span>
					{/if}
					<span
						class="absolute inset-x-0 bottom-0 truncate bg-black/75 px-1.5 py-1 text-[9px] font-semibold text-white"
					>
						{eventLabel(cluster.event.kind)}
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

			{#if playheadTop !== null && displayedPlayheadMs !== null}
				<button
					type="button"
					class="absolute right-0 left-0 z-40 flex h-7 -translate-y-1/2 touch-none items-center select-none focus-visible:ring-2 focus-visible:ring-red-500 focus-visible:outline-none {dragPointerId ===
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
