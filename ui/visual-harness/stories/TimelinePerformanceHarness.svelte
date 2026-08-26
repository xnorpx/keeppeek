<script lang="ts">
	import HorizontalTimeline from '$lib/components/HorizontalTimeline.svelte';
	import VerticalTimeline from '$lib/components/VerticalTimeline.svelte';
	import type { TimelineViewport } from '$lib/timeline-repository.svelte';
	import type { RecordingEvent, RecordingSegment } from '$lib/types';

	type Props = {
		orientation?: 'horizontal' | 'vertical';
	};

	let { orientation = 'vertical' }: Props = $props();

	const date = '2026-08-18';
	const dayStartMs = Date.parse(`${date}T00:00:00Z`);
	const minuteMs = 60_000;
	const dayMs = 24 * 60 * minuteMs;
	const dayEndMs = dayStartMs + dayMs;
	const segmentCount = 24 * 60;
	const eventCount = 600;
	const segments: RecordingSegment[] = Array.from({ length: segmentCount }, (_, index) => {
		const startTimeMs = dayStartMs + index * minuteMs;
		return {
			stream: 'main',
			date,
			hour: Math.floor(index / 60)
				.toString()
				.padStart(2, '0'),
			filename: `${index}.mp4`,
			url: `/timeline-performance/${index}.mp4`,
			start_time_ms: startTimeMs,
			end_time_ms: startTimeMs + minuteMs,
			duration_ms: minuteMs
		};
	});
	const eventKinds = ['person', 'vehicle', 'motion'] as const;
	const events: RecordingEvent[] = Array.from({ length: eventCount }, (_, index) => {
		const startTimeMs = dayStartMs + Math.floor((index * dayMs) / eventCount);
		return {
			id: `event-${index}`,
			source: index % 7 === 0 ? 'keeppeek' : 'camera',
			kind: eventKinds[index % eventKinds.length],
			start_time_ms: startTimeMs,
			end_time_ms: startTimeMs + 10_000,
			confidence: 0.75 + (index % 20) / 100,
			bbox: null,
			zone: null,
			thumbnail_url: null
		};
	});

	let playheadMs = $state(dayEndMs - 30 * minuteMs);
	let seekCount = $state(0);
	let scrubCount = $state(0);
	let viewportChangeCount = $state(0);

	function seek(timestampMs: number): void {
		playheadMs = timestampMs;
		seekCount += 1;
	}

	function scrub(timestampMs: number): void {
		playheadMs = timestampMs;
		scrubCount += 1;
	}

	function viewportChanged(_viewport: TimelineViewport): void {
		viewportChangeCount += 1;
	}
</script>

<main
	data-timeline-performance-harness
	data-segment-count={segmentCount}
	data-event-count={eventCount}
	data-seek-count={seekCount}
	data-scrub-count={scrubCount}
	data-viewport-change-count={viewportChangeCount}
	class="flex h-screen w-screen items-start justify-center overflow-hidden bg-ground"
>
	{#if orientation === 'horizontal'}
		<div class="w-full pt-24">
			<HorizontalTimeline
				{segments}
				{events}
				selectedUrl={null}
				{playheadMs}
				{dayStartMs}
				nowMs={dayEndMs}
				onSeek={seek}
				onScrubStart={scrub}
				onScrub={scrub}
				onScrubEnd={scrub}
				onViewportChange={viewportChanged}
			/>
		</div>
	{:else}
		<VerticalTimeline
			{segments}
			{events}
			selectedUrl={null}
			{playheadMs}
			{dayStartMs}
			nowMs={dayEndMs}
			onSeek={seek}
			onScrubStart={scrub}
			onScrub={scrub}
			onScrubEnd={scrub}
			onViewportChange={viewportChanged}
		/>
	{/if}
</main>
