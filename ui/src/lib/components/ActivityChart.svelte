<script lang="ts">
	import {
		buildEventsDrilldownHref,
		formatCompletenessLabel,
		formatDurationMs,
		type ActivitySummaryData,
		type ActivitySummaryFilterOptions,
		type ActivityTimeBucket
	} from '$lib/activity-analytics';

	let {
		summary,
		filter,
		eventsPath = '/events'
	}: {
		summary: ActivitySummaryData;
		filter: ActivitySummaryFilterOptions;
		eventsPath?: string;
	} = $props();

	let viewMode = $state<'chart' | 'table'>('chart');
	let maxBucketCount = $derived(
		Math.max(1, ...summary.buckets.map((bucket) => bucket.measures.eventCount))
	);

	function bucketHref(bucket: ActivityTimeBucket): string | null {
		const link = buildEventsDrilldownHref(
			summary,
			filter,
			{ startMs: bucket.startMs, endMs: bucket.endMs },
			eventsPath
		);
		return link.ok ? link.href : null;
	}

	function completenessBadge(bucket: ActivityTimeBucket): string {
		if (bucket.completeness === 'partial_retention') return 'PARTIAL RETENTION';
		if (bucket.completeness === 'source_gap') return 'SOURCE GAP';
		if (bucket.completeness === 'partial_current') return 'IN PROGRESS';
		if (bucket.completeness === 'unknown') return 'UNKNOWN';
		return 'COMPLETE';
	}
</script>

<section
	class="rounded-sm border border-border bg-surface p-4"
	aria-labelledby="activity-trend-heading"
>
	<div class="mb-3 flex flex-wrap items-center justify-between gap-2">
		<div>
			<h2 id="activity-trend-heading" class="text-sm font-semibold">
				Reported event activity over time
			</h2>
			<p class="text-xs text-text-muted">
				Timezone: {summary.timezone} (tzdata {summary.tzdbVersion}) · UTC bounds:
				{new Date(summary.startMs).toISOString()} to {new Date(summary.endMs).toISOString()}
			</p>
		</div>
		<div class="inline-flex rounded-sm border border-border" role="group" aria-label="Trend view">
			<button
				type="button"
				class="min-h-11 px-3 text-xs focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {viewMode ===
				'chart'
					? 'bg-raised font-medium'
					: ''}"
				aria-pressed={viewMode === 'chart'}
				onclick={() => (viewMode = 'chart')}
			>
				Chart
			</button>
			<button
				type="button"
				class="min-h-11 border-l border-border px-3 text-xs focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {viewMode ===
				'table'
					? 'bg-raised font-medium'
					: ''}"
				aria-pressed={viewMode === 'table'}
				onclick={() => (viewMode = 'table')}
			>
				Table
			</button>
		</div>
	</div>

	{#if summary.buckets.length === 0}
		<p class="py-6 text-center text-xs text-text-muted">
			No reported event buckets match the selected filters.
		</p>
	{:else if viewMode === 'chart'}
		<ol
			class="flex max-h-80 flex-col gap-1.5 overflow-y-auto pr-1"
			aria-label="Activity buckets chart"
		>
			{#each summary.buckets as bucket (`${bucket.startMs}-${bucket.label}-${bucket.dstFold}`)}
				{@const href = bucketHref(bucket)}
				{@const widthPct = Math.round((bucket.measures.eventCount / maxBucketCount) * 100)}
				<li class="flex flex-wrap items-center gap-2 text-xs">
					<span class="w-44 shrink-0 font-mono text-[11px]">{bucket.label}</span>
					<div class="bg-bg relative h-6 min-w-32 flex-1 rounded-xs border border-border">
						<div
							class="h-full rounded-xs {bucket.completeness === 'complete'
								? 'bg-accent/70'
								: 'bg-accent/40'}"
							style="width: {widthPct}%; {bucket.completeness !== 'complete'
								? 'background-image: repeating-linear-gradient(45deg, transparent, transparent 4px, rgba(255,255,255,0.2) 4px, rgba(255,255,255,0.2) 8px);'
								: ''}"
						></div>
					</div>
					<span class="w-20 shrink-0 text-right font-mono tabular-nums">
						{bucket.measures.eventCount} events
					</span>
					<span class="w-32 shrink-0 text-[10px] tracking-wide text-text-muted uppercase">
						{completenessBadge(bucket)}
					</span>
					{#if href}
						<a
							{href}
							class="inline-flex min-h-11 shrink-0 items-center rounded-sm border border-border px-2.5 text-xs hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						>
							Open events
						</a>
					{:else}
						<span class="text-[11px] text-text-muted">Range &gt; 31d</span>
					{/if}
				</li>
			{/each}
		</ol>
	{:else}
		<div class="overflow-x-auto">
			<table class="w-full border-collapse text-left text-xs">
				<caption class="sr-only">Reported event counts per time bucket</caption>
				<thead>
					<tr class="border-b border-border text-text-muted">
						<th scope="col" class="py-2 pr-3 font-medium">Bucket ({summary.timezone})</th>
						<th scope="col" class="py-2 pr-3 font-medium">UTC window</th>
						<th scope="col" class="py-2 pr-3 font-medium">Reported events</th>
						<th scope="col" class="py-2 pr-3 font-medium">Avg duration</th>
						<th scope="col" class="py-2 pr-3 font-medium">Completeness</th>
						<th scope="col" class="py-2 font-medium">Events</th>
					</tr>
				</thead>
				<tbody>
					{#each summary.buckets as bucket (`table-${bucket.startMs}-${bucket.label}-${bucket.dstFold}`)}
						{@const href = bucketHref(bucket)}
						<tr class="border-b border-border/60">
							<th scope="row" class="py-2 pr-3 font-mono font-normal">{bucket.label}</th>
							<td class="py-2 pr-3 font-mono text-[11px] text-text-muted">
								{new Date(bucket.startMs).toISOString()} – {new Date(bucket.endMs).toISOString()}
							</td>
							<td class="py-2 pr-3 font-mono tabular-nums">{bucket.measures.eventCount}</td>
							<td class="py-2 pr-3 font-mono tabular-nums">
								{formatDurationMs(bucket.measures.averageDurationMs)}
							</td>
							<td class="py-2 pr-3">{formatCompletenessLabel(bucket.completeness)}</td>
							<td class="py-2">
								{#if href}
									<a
										{href}
										class="inline-flex min-h-11 items-center underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
									>
										Open matching events
									</a>
								{:else}
									<span class="text-text-muted">Narrow range to ≤ 31d</span>
								{/if}
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</section>
