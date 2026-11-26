<script lang="ts">
	import {
		buildEventsDrilldownHref,
		formatDurationMs,
		type ActivityBreakdownRow,
		type ActivitySummaryData,
		type ActivitySummaryFilterOptions,
		type EventsDrilldownOverride
	} from '$lib/activity-analytics';

	let {
		title,
		rows,
		summary,
		filter,
		rowOverride,
		eventsPath = '/events'
	}: {
		title: string;
		rows: readonly ActivityBreakdownRow[];
		summary: ActivitySummaryData;
		filter: ActivitySummaryFilterOptions;
		rowOverride: (row: ActivityBreakdownRow) => EventsDrilldownOverride;
		eventsPath?: string;
	} = $props();
</script>

<section class="rounded-sm border border-border bg-surface p-4">
	<h3 class="mb-2 text-sm font-semibold">{title}</h3>
	{#if rows.length === 0}
		<p class="py-4 text-xs text-text-muted">No reported events in this breakdown.</p>
	{:else}
		<div class="overflow-x-auto">
			<table class="w-full border-collapse text-left text-xs">
				<caption class="sr-only">{title} ranked reported event counts</caption>
				<thead>
					<tr class="border-b border-border text-text-muted">
						<th scope="col" class="py-1.5 pr-3 font-medium">Item</th>
						<th scope="col" class="py-1.5 pr-3 text-right font-medium">Reported events</th>
						<th scope="col" class="py-1.5 pr-3 text-right font-medium">Share</th>
						<th scope="col" class="py-1.5 pr-3 text-right font-medium">Avg duration</th>
						<th scope="col" class="py-1.5 text-right font-medium">Events</th>
					</tr>
				</thead>
				<tbody>
					{#each rows as row (`${title}-${row.key}-${row.isUnknown}`)}
						{@const link = buildEventsDrilldownHref(summary, filter, rowOverride(row), eventsPath)}
						<tr class="border-b border-border/60">
							<th scope="row" class="py-1.5 pr-3 font-normal">
								{row.label}
								{#if row.isUnknown}
									<span
										class="ml-1 rounded-xs border border-border px-1 text-[10px] text-text-muted"
									>
										Unknown metadata
									</span>
								{/if}
							</th>
							<td class="py-1.5 pr-3 text-right font-mono tabular-nums">
								{row.measures.eventCount}
							</td>
							<td class="py-1.5 pr-3 text-right font-mono tabular-nums">
								{row.sharePercent.toFixed(1)}%
							</td>
							<td class="py-1.5 pr-3 text-right font-mono tabular-nums">
								{formatDurationMs(row.measures.averageDurationMs)}
							</td>
							<td class="py-1.5 text-right">
								{#if link.ok}
									<a
										href={link.href}
										class="inline-flex min-h-11 items-center underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
									>
										Open
									</a>
								{:else}
									<span class="text-[11px] text-text-muted" title={link.reason}>Unavailable</span>
								{/if}
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</section>
