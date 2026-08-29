<script lang="ts">
	import { formatPercent, rangePosition } from '$lib/recording-coverage';
	import type { StreamRecordingCoverage } from '$lib/types';

	type Props = {
		stream: StreamRecordingCoverage;
		windowStartMs: number;
		windowEndMs: number;
	};

	let { stream, windowStartMs, windowEndMs }: Props = $props();
	let useBuckets = $derived(windowEndMs - windowStartMs > 86_400_000 && stream.buckets.length > 0);
	let segments = $derived(
		useBuckets
			? stream.buckets.map((bucket) => ({
					...bucket,
					...rangePosition(bucket.start_ms, bucket.end_ms, windowStartMs, windowEndMs),
					opacity: Math.max(0.2, bucket.coverage_ms / Math.max(1, bucket.end_ms - bucket.start_ms))
				}))
			: stream.ranges.map((range) => ({
					...range,
					...rangePosition(range.start_ms, range.end_ms, windowStartMs, windowEndMs),
					opacity: 1
				}))
	);
	let startLabel = $derived(formatBoundary(windowStartMs));
	let midpointLabel = $derived(formatBoundary(windowStartMs + (windowEndMs - windowStartMs) / 2));
	let endLabel = $derived(formatBoundary(windowEndMs));

	function formatBoundary(timestampMs: number): string {
		return new Intl.DateTimeFormat(undefined, {
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit'
		}).format(new Date(timestampMs));
	}
</script>

<div
	data-recording-coverage-strip
	class="space-y-2"
	role="img"
	aria-label={`${stream.stream_id} stream has ${formatPercent(stream.coverage_percent)} playable indexed coverage and ${stream.gap_count} gaps in the selected interval`}
>
	<div class="relative h-9 overflow-hidden rounded-sm border border-hairline-strong bg-live/15">
		{#each segments as segment (`${segment.start_ms}-${segment.end_ms}`)}
			<span
				class="absolute inset-y-0 bg-availability"
				style={`left:${segment.left}%;width:${Math.max(segment.width, 0.15)}%;opacity:${segment.opacity}`}
			></span>
		{/each}
		<span class="absolute inset-y-0 left-1/2 w-px bg-foreground/15"></span>
	</div>
	<div class="grid grid-cols-3 font-mono text-[10px] text-text-faint tabular-nums">
		<span>{startLabel}</span>
		<span class="text-center">{midpointLabel}</span>
		<span class="text-right">{endLabel}</span>
	</div>
	<div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-text-muted">
		<span class="inline-flex items-center gap-1.5"
			><span class="h-1.5 w-4 bg-availability"></span>Playable</span
		>
		<span class="inline-flex items-center gap-1.5"
			><span class="h-1.5 w-4 bg-live/40"></span>Gap</span
		>
		{#if useBuckets}<span
				>{Math.round(stream.bucket_ms / 3_600_000)}h buckets · exact totals retained</span
			>{:else if stream.detail_truncated}<span>Recent detail shown · exact totals retained</span
			>{/if}
	</div>
</div>
