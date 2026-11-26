<script lang="ts">
	import ActivityChart from './ActivityChart.svelte';
	import ActivityTable from './ActivityTable.svelte';
	import {
		exportActivitySummaryCsv,
		exportActivitySummaryJson,
		formatCompletenessLabel,
		formatConfidenceBandLabel,
		formatDurationMs,
		isEventConfidenceBand,
		type ActivityAnalyticsStatusData,
		type ActivityBucketGranularity,
		type ActivityComparisonMode,
		type ActivitySummaryData,
		type ActivitySummaryFilterOptions,
		type ActivityWeekStart,
		type EventConfidenceBand
	} from '$lib/activity-analytics';
	import type { CameraListItem, RecordingEvent } from '$lib/types';
	import type { EventImageFilter } from '$lib/event-browser';
	import type { EventReviewFilter } from '$lib/event-workflow';

	const WEEKDAY_LABELS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'] as const;
	const TIMEZONE_OPTIONS = [
		'UTC',
		'America/Los_Angeles',
		'America/Denver',
		'America/Chicago',
		'America/New_York',
		'Europe/London',
		'Europe/Berlin',
		'Asia/Kolkata',
		'Asia/Tokyo',
		'Australia/Sydney'
	] as const;

	export type ActivityUiControls = {
		rangePreset: '24h' | '7d' | '30d';
		granularity: ActivityBucketGranularity;
		timezone: string;
		weekStart: ActivityWeekStart;
		comparison: ActivityComparisonMode;
		cameraId: string;
		eventType: string;
		origin: '' | RecordingEvent['source'];
		zone: string;
		missingZone: boolean;
		confidenceBand: '' | EventConfidenceBand;
		image: EventImageFilter;
		review: EventReviewFilter;
		bookmarks: 'all' | 'bookmarked' | 'mine';
	};

	let {
		summary,
		status,
		cameras,
		controls,
		loading = false,
		error = null,
		isAdmin = false,
		rebuilding = false,
		eventsPath = '/events',
		onControlsChange,
		onRefresh,
		onRebuild,
		onExport
	}: {
		summary: ActivitySummaryData | null;
		status: ActivityAnalyticsStatusData | null;
		cameras: readonly CameraListItem[];
		controls: ActivityUiControls;
		loading?: boolean;
		error?: string | null;
		isAdmin?: boolean;
		rebuilding?: boolean;
		eventsPath?: string;
		onControlsChange: (next: ActivityUiControls) => void;
		onRefresh: () => void;
		onRebuild?: () => void;
		onExport?: (format: 'csv' | 'json', filename: string, content: string, mime: string) => void;
	} = $props();

	let exportError = $state<string | null>(null);

	let activeFilter = $derived<ActivitySummaryFilterOptions>({
		sourceIds: controls.cameraId ? [controls.cameraId] : [],
		eventTypes: controls.eventType ? [controls.eventType] : [],
		origins: controls.origin ? [controls.origin] : [],
		zones: controls.zone && !controls.missingZone ? [controls.zone] : [],
		includeMissingZone: controls.missingZone,
		confidenceBands: controls.confidenceBand ? [controls.confidenceBand] : [],
		image: controls.image,
		review: controls.review,
		bookmarks: controls.bookmarks
	});

	let availableEventTypes = $derived.by(() => {
		const set = new Set<string>();
		for (const row of summary?.byEventType ?? []) {
			if (row.key) set.add(row.key);
		}
		if (controls.eventType) set.add(controls.eventType);
		return [...set].toSorted();
	});

	let availableZones = $derived.by(() => {
		const set = new Set<string>();
		for (const row of summary?.byZone ?? []) {
			if (!row.isUnknown && row.key) set.add(row.key);
		}
		if (controls.zone) set.add(controls.zone);
		return [...set].toSorted();
	});

	let timezoneChoices = $derived.by(() => {
		const set = new Set<string>(TIMEZONE_OPTIONS);
		if (status?.siteTimezone) set.add(status.siteTimezone);
		if (controls.timezone) set.add(controls.timezone);
		return [...set];
	});

	let heatmapGrid = $derived.by(() => {
		if (!summary || summary.heatmap.length === 0) return null;
		const rows: number[][] = Array.from({ length: 7 }, () => Array.from({ length: 24 }, () => 0));
		let peak = 1;
		for (const cell of summary.heatmap) {
			if (cell.weekday >= 1 && cell.weekday <= 7 && cell.localHour >= 0 && cell.localHour < 24) {
				rows[cell.weekday - 1]![cell.localHour] = cell.eventCount;
				if (cell.eventCount > peak) peak = cell.eventCount;
			}
		}
		return { rows, peak };
	});

	function update(patch: Partial<ActivityUiControls>): void {
		onControlsChange({ ...controls, ...patch });
	}

	function triggerExport(format: 'csv' | 'json'): void {
		if (!summary) return;
		exportError = null;
		try {
			const content =
				format === 'csv' ? exportActivitySummaryCsv(summary) : exportActivitySummaryJson(summary);
			const mime = format === 'csv' ? 'text/csv;charset=utf-8' : 'application/json;charset=utf-8';
			const stamp = new Date(summary.generatedAtMs).toISOString().replaceAll(':', '-');
			const filename = `keeppeek-activity-${stamp}.${format}`;
			if (onExport) {
				onExport(format, filename, content, mime);
				return;
			}
			const blob = new Blob([content], { type: mime });
			const url = URL.createObjectURL(blob);
			const anchor = document.createElement('a');
			anchor.href = url;
			anchor.download = filename;
			anchor.click();
			URL.revokeObjectURL(url);
		} catch (cause) {
			exportError = cause instanceof Error ? cause.message : 'Unable to export activity summary.';
		}
	}

	function formatPct(value: number | null): string {
		return value === null ? 'N/A' : `${value.toFixed(1)}%`;
	}

	function groupCameraIds(groupId: string): string[] {
		return cameras.filter((cam) => cam.group_ids?.includes(groupId)).map((cam) => cam.id);
	}
</script>

<div class="flex flex-col gap-4">
	<header class="flex flex-wrap items-center justify-between gap-3">
		<div>
			<h1 class="text-xl font-semibold">Activity summary</h1>
			<p class="text-xs text-text-muted">
				Bounded temporal summaries of producer-reported event metadata.
			</p>
		</div>
		<div class="flex flex-wrap items-center gap-2">
			<a
				href={eventsPath}
				class="inline-flex min-h-11 items-center rounded-sm border border-border px-3 text-xs hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			>
				Back to Events
			</a>
			<button
				type="button"
				class="min-h-11 rounded-sm border border-border px-3 text-xs hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				disabled={loading}
				onclick={onRefresh}
			>
				{loading ? 'Refreshing...' : 'Refresh'}
			</button>
			<button
				type="button"
				class="min-h-11 rounded-sm border border-border px-3 text-xs hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				disabled={!summary}
				onclick={() => triggerExport('csv')}
			>
				Export CSV
			</button>
			<button
				type="button"
				class="min-h-11 rounded-sm border border-border px-3 text-xs hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				disabled={!summary}
				onclick={() => triggerExport('json')}
			>
				Export JSON
			</button>
			{#if isAdmin && onRebuild}
				<button
					type="button"
					class="min-h-11 rounded-sm border border-border px-3 text-xs hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					disabled={rebuilding}
					onclick={onRebuild}
				>
					{rebuilding ? 'Rebuilding...' : 'Rebuild rollups'}
				</button>
			{/if}
		</div>
	</header>

	<p
		role="note"
		class="rounded-sm border border-border bg-raised/50 px-3 py-2 text-xs text-text-muted"
	>
		{summary?.provenanceNotice ??
			'Counts reflect producer-reported event metadata within current retention and authorization scope, not authoritative real-world identity, occupancy, or threat totals.'}
	</p>

	<section
		aria-label="Activity summary filters"
		class="grid grid-cols-1 gap-2 rounded-sm border border-border bg-surface p-3 lg:grid-cols-4 sm:grid-cols-2"
	>
		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Window</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.rangePreset}
				onchange={(e) =>
					update({ rangePreset: e.currentTarget.value as ActivityUiControls['rangePreset'] })}
			>
				<option value="24h">Last 24 hours</option>
				<option value="7d">Last 7 days</option>
				<option value="30d">Last 30 days</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Granularity</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.granularity}
				onchange={(e) =>
					update({ granularity: e.currentTarget.value as ActivityBucketGranularity })}
			>
				<option value="hour">Hour</option>
				<option value="day">Day</option>
				<option value="week">Week</option>
				<option value="month">Month</option>
				<option value="hour_of_day">Hour of day (0-23)</option>
				<option value="weekday_weekend">Weekday vs weekend</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Display timezone (IANA)</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.timezone || status?.siteTimezone || 'UTC'}
				onchange={(e) => update({ timezone: e.currentTarget.value })}
			>
				{#each timezoneChoices as zone (zone)}
					<option value={zone}>{zone}</option>
				{/each}
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Comparison</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.comparison}
				onchange={(e) => update({ comparison: e.currentTarget.value as ActivityComparisonMode })}
			>
				<option value="previous_duration">Previous equal duration</option>
				<option value="previous_calendar">Previous calendar window</option>
				<option value="none">No comparison</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Camera</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.cameraId}
				onchange={(e) => update({ cameraId: e.currentTarget.value })}
			>
				<option value="">All authorized cameras</option>
				{#each cameras as camera (camera.id)}
					<option value={camera.id}>{camera.name ?? camera.id}</option>
				{/each}
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Event origin</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.origin}
				onchange={(e) => update({ origin: e.currentTarget.value as '' | RecordingEvent['source'] })}
			>
				<option value="">Any origin</option>
				<option value="camera">Camera</option>
				<option value="keeppeek">KeepPeek / service</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Confidence band</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.confidenceBand}
				onchange={(e) => {
					const val = e.currentTarget.value;
					update({ confidenceBand: isEventConfidenceBand(val) ? val : '' });
				}}
			>
				<option value="">Any confidence</option>
				<option value="high">{formatConfidenceBandLabel('high')}</option>
				<option value="medium">{formatConfidenceBandLabel('medium')}</option>
				<option value="low">{formatConfidenceBandLabel('low')}</option>
				<option value="unknown">{formatConfidenceBandLabel('unknown')}</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Week start</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.weekStart}
				onchange={(e) => update({ weekStart: e.currentTarget.value as ActivityWeekStart })}
			>
				<option value="monday">Monday</option>
				<option value="sunday">Sunday</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Event type</span>
			<input
				type="text"
				list="activity-event-types"
				placeholder="Any type"
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.eventType}
				onchange={(e) => update({ eventType: e.currentTarget.value.trim() })}
			/>
			<datalist id="activity-event-types">
				{#each availableEventTypes as kind (kind)}
					<option value={kind}></option>
				{/each}
			</datalist>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Zone</span>
			<input
				type="text"
				list="activity-zones"
				placeholder={controls.missingZone ? 'Missing zone only' : 'Any zone'}
				disabled={controls.missingZone}
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs disabled:opacity-50"
				value={controls.zone}
				onchange={(e) => update({ zone: e.currentTarget.value.trim(), missingZone: false })}
			/>
			<datalist id="activity-zones">
				{#each availableZones as zoneName (zoneName)}
					<option value={zoneName}></option>
				{/each}
			</datalist>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Zone presence</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.missingZone ? 'missing' : 'any'}
				onchange={(e) => {
					const missingZone = e.currentTarget.value === 'missing';
					update({ missingZone, ...(missingZone ? { zone: '' } : {}) });
				}}
			>
				<option value="any">Any zone state</option>
				<option value="missing">Missing zone only</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Image</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.image}
				onchange={(e) => update({ image: e.currentTarget.value as EventImageFilter })}
			>
				<option value="all">Any image</option>
				<option value="with">With images</option>
				<option value="without">Without images</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Review state</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.review}
				onchange={(e) => update({ review: e.currentTarget.value as EventReviewFilter })}
			>
				<option value="all">All review states</option>
				<option value="unreviewed">Unreviewed</option>
				<option value="reviewed">Reviewed</option>
				<option value="dismissed">Dismissed</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-xs">
			<span class="text-text-muted">Bookmarks</span>
			<select
				class="bg-bg min-h-11 rounded-sm border border-border px-2 text-xs"
				value={controls.bookmarks}
				onchange={(e) =>
					update({ bookmarks: e.currentTarget.value as ActivityUiControls['bookmarks'] })}
			>
				<option value="all">All events</option>
				<option value="bookmarked">Bookmarked</option>
				<option value="mine">Bookmarked by me</option>
			</select>
		</label>
	</section>

	{#if error}
		<div role="alert" class="rounded-sm border border-destructive bg-destructive/10 p-3 text-xs">
			{error}
		</div>
	{/if}

	{#if exportError}
		<div role="alert" class="rounded-sm border border-destructive bg-destructive/10 p-3 text-xs">
			{exportError}
		</div>
	{/if}

	{#if summary}
		{#if summary.retention.partialRetention || summary.sourceHealth.missingHealthCameras > 0 || summary.rollupState !== 'ready'}
			<div
				role="status"
				class="flex flex-col gap-1 rounded-sm border border-border bg-raised px-3 py-2 text-xs"
			>
				{#if summary.retention.partialRetention}
					<p>
						<strong>Partial retention:</strong>
						{summary.retention.deletedEventCountInRange} historical reported events were deleted inside
						this window.
					</p>
				{/if}
				{#if summary.sourceHealth.missingHealthCameras > 0}
					<p>
						<strong>Event-source health gap:</strong>
						{summary.sourceHealth.missingHealthCameras} of {summary.sourceHealth.authorizedCameras}
						authorized cameras had missing expected event-source health during this window.
					</p>
				{/if}
				{#if summary.rollupState !== 'ready'}
					<p>
						<strong>Rollup state:</strong>
						{summary.rollupState} (data source: {summary.dataSource}).
					</p>
				{/if}
			</div>
		{/if}

		<section
			aria-label="Overview metrics"
			class="grid grid-cols-1 gap-3 lg:grid-cols-4 sm:grid-cols-2"
		>
			<div class="rounded-sm border border-border bg-surface p-3">
				<p class="text-xs text-text-muted">Reported events</p>
				<p class="mt-1 font-mono text-2xl font-semibold tabular-nums">
					{summary.totals.eventCount}
				</p>
				<p class="text-[11px] text-text-muted">
					Distinct current IDs: {summary.totals.distinctEventCount}
				</p>
				{#if summary.comparison}
					<p class="mt-1 text-[11px] text-text-muted">
						vs prior ({summary.comparison.totals.eventCount}):
						{summary.comparison.eventCountDelta >= 0 ? '+' : ''}{summary.comparison.eventCountDelta}
						{#if summary.comparison.eventCountDeltaPercent !== null}
							({summary.comparison.eventCountDeltaPercent.toFixed(1)}%)
						{/if}
						· {formatCompletenessLabel(summary.comparison.completeness)}
					</p>
				{/if}
			</div>

			<div class="rounded-sm border border-border bg-surface p-3">
				<p class="text-xs text-text-muted">Interval duration</p>
				<p class="mt-1 font-mono text-2xl font-semibold tabular-nums">
					{formatDurationMs(summary.totals.averageDurationMs)}
				</p>
				<p class="text-[11px] text-text-muted">
					Across {summary.totals.intervalEventCount} interval events ({summary.totals
						.openIntervalEventCount} open clipped at snapshot)
				</p>
			</div>

			<div class="rounded-sm border border-border bg-surface p-3">
				<p class="text-xs text-text-muted">Metadata coverage</p>
				<p class="mt-1 text-xs">
					Image: <strong class="font-mono">{formatPct(summary.totals.percentWithImage)}</strong>
					({summary.totals.withImageCount}/{summary.totals.eventCount})
				</p>
				<p class="text-xs">
					Confidence:
					<strong class="font-mono">{formatPct(summary.totals.percentWithConfidence)}</strong>
					({summary.totals.withConfidenceCount}/{summary.totals.eventCount})
				</p>
				<p class="text-xs">
					Zone: <strong class="font-mono">{formatPct(summary.totals.percentWithZone)}</strong>
					({summary.totals.withZoneCount}/{summary.totals.eventCount})
				</p>
			</div>

			<div class="rounded-sm border border-border bg-surface p-3">
				<p class="text-xs text-text-muted">Camera event-source health</p>
				<p class="mt-1 text-xs">
					Reporting: <strong class="font-mono">{summary.sourceHealth.reportingCameras}</strong> /
					{summary.sourceHealth.authorizedCameras}
				</p>
				<p class="text-xs">
					Healthy (no reports): {summary.sourceHealth.healthyNoReportsCameras} · Missing health:
					{summary.sourceHealth.missingHealthCameras}
				</p>
				<p class="text-[11px] text-text-muted">
					Unknown history: {summary.sourceHealth.unknownHistoryCameras} · No expected source:
					{summary.sourceHealth.noExpectedSourceCameras}
				</p>
			</div>
		</section>

		<ActivityChart {summary} filter={activeFilter} {eventsPath} />

		{#if heatmapGrid}
			<section
				class="rounded-sm border border-border bg-surface p-4"
				aria-labelledby="activity-heatmap-heading"
			>
				<h2 id="activity-heatmap-heading" class="mb-2 text-sm font-semibold">
					Weekday × local hour heatmap ({summary.timezone})
				</h2>
				<div class="overflow-x-auto">
					<table class="w-full border-collapse text-center text-[11px]">
						<caption class="sr-only">
							Reported event counts by weekday and local hour in {summary.timezone}
						</caption>
						<thead>
							<tr>
								<th scope="col" class="p-1 text-left font-medium text-text-muted">Day</th>
								{#each Array.from({ length: 24 }, (_, hour) => hour) as hour (hour)}
									<th scope="col" class="p-1 font-mono font-normal text-text-muted">
										{String(hour).padStart(2, '0')}
									</th>
								{/each}
							</tr>
						</thead>
						<tbody>
							{#each WEEKDAY_LABELS as dayLabel, dayIndex (dayLabel)}
								<tr>
									<th scope="row" class="p-1 text-left font-medium">{dayLabel}</th>
									{#each heatmapGrid.rows[dayIndex] ?? [] as count, hour (`${dayIndex}-${hour}`)}
										{@const intensity = count === 0 ? 0 : Math.max(0.15, count / heatmapGrid.peak)}
										<td
											class="border border-border/40 p-1 font-mono tabular-nums"
											style="background-color: color-mix(in srgb, var(--color-accent, #3b82f6) {Math.round(
												intensity * 65
											)}%, transparent);"
											aria-label="{dayLabel} {String(hour).padStart(
												2,
												'0'
											)}:00 — {count} reported events"
										>
											{count}
										</td>
									{/each}
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			</section>
		{/if}

		<div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
			<ActivityTable
				title="Cameras"
				rows={summary.byCamera}
				{summary}
				filter={activeFilter}
				{eventsPath}
				rowOverride={(row) => ({ cameraId: row.key })}
			/>
			<ActivityTable
				title="Event types"
				rows={summary.byEventType}
				{summary}
				filter={activeFilter}
				{eventsPath}
				rowOverride={(row) => ({ type: row.key })}
			/>
			<ActivityTable
				title="Zones"
				rows={summary.byZone}
				{summary}
				filter={activeFilter}
				{eventsPath}
				rowOverride={(row) => (row.isUnknown ? { missingZone: true } : { zone: row.key })}
			/>
			<ActivityTable
				title="Confidence bands"
				rows={summary.byConfidenceBand}
				{summary}
				filter={activeFilter}
				{eventsPath}
				rowOverride={(row) => ({
					confidenceBand: isEventConfidenceBand(row.key) ? row.key : 'unknown'
				})}
			/>
			<ActivityTable
				title="Origins / sources"
				rows={summary.byOrigin}
				{summary}
				filter={activeFilter}
				{eventsPath}
				rowOverride={(row) => ({
					source: row.key === 'keeppeek' ? 'keeppeek' : 'camera'
				})}
			/>
			<ActivityTable
				title="Camera groups"
				rows={summary.byGroup}
				{summary}
				filter={activeFilter}
				{eventsPath}
				rowOverride={(row) => ({ cameraIds: groupCameraIds(row.key) })}
			/>
		</div>
	{/if}
</div>
