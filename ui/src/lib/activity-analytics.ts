import type { RecordingEvent } from './types';
import type { EventImageFilter } from './event-browser';
import type { EventReviewFilter } from './event-workflow';

export const ACTIVITY_ANALYTICS_CAPABILITY = 'keeppeek.activity-analytics.v1' as const;
export const MAX_ACTIVITY_EXPORT_ROWS = 25_000;
export const MAX_ACTIVITY_EXPORT_BYTES = 1_048_576;
export const MAX_EVENTS_DRILLDOWN_WINDOW_MS = 31 * 86_400_000;

export type EventConfidenceBand = 'unknown' | 'low' | 'medium' | 'high';

export type ActivityBucketGranularity =
	'hour' | 'day' | 'week' | 'month' | 'hour_of_day' | 'weekday_weekend';

export type ActivityWeekStart = 'monday' | 'sunday';

export type ActivityDimension =
	'camera' | 'group' | 'event_type' | 'origin' | 'zone' | 'confidence_band' | 'image' | 'workflow';

export type ActivityComparisonMode = 'none' | 'previous_duration' | 'previous_calendar';

export type ActivityRollupState = 'ready' | 'rebuilding' | 'degraded' | 'unavailable';

export type ActivityCompletenessState =
	'complete' | 'partial_current' | 'partial_retention' | 'source_gap' | 'unknown';

export type ActivityCameraHealthStatus =
	'reporting' | 'healthy_no_reports' | 'missing_health' | 'no_expected_source' | 'unknown_history';

export type ActivitySummaryFilterOptions = {
	sourceIds?: readonly string[];
	groupIds?: readonly string[];
	eventTypes?: readonly string[];
	origins?: readonly RecordingEvent['source'][];
	zones?: readonly string[];
	includeMissingZone?: boolean;
	confidenceBands?: readonly EventConfidenceBand[];
	image?: EventImageFilter;
	review?: EventReviewFilter;
	bookmarks?: 'all' | 'bookmarked' | 'mine';
};

export type ActivitySummaryQueryOptions = {
	startMs: number;
	endMs: number;
	timezone?: string;
	granularity: ActivityBucketGranularity;
	weekStart?: ActivityWeekStart;
	filter?: ActivitySummaryFilterOptions;
	groupBy?: readonly ActivityDimension[];
	comparison?: ActivityComparisonMode;
	denseBuckets?: boolean;
	maxBuckets?: number;
	maxSeries?: number;
	maxRankedRows?: number;
	includeHeatmap?: boolean;
	signal?: AbortSignal;
};

export type ActivityMeasures = {
	eventCount: number;
	distinctEventCount: number;
	intervalEventCount: number;
	openIntervalEventCount: number;
	totalDurationMs: number;
	averageDurationMs: number | null;
	withImageCount: number;
	withConfidenceCount: number;
	withZoneCount: number;
	percentWithImage: number | null;
	percentWithConfidence: number | null;
	percentWithZone: number | null;
};

export type ActivityTimeBucket = {
	startMs: number;
	endMs: number;
	label: string;
	utcOffsetSeconds: number;
	dstFold: number;
	partialCurrent: boolean;
	completeness: ActivityCompletenessState;
	retentionPartial: boolean;
	sourceHealthGap: boolean;
	measures: ActivityMeasures;
};

export type ActivitySeries = {
	dimensions: Partial<Record<ActivityDimension, string>>;
	label: string;
	totals: ActivityMeasures;
	bucketCounts: number[];
};

export type ActivityHeatmapCell = {
	weekday: number;
	localHour: number;
	eventCount: number;
	distinctEventCount: number;
	completeness: ActivityCompletenessState;
};

export type ActivityBreakdownRow = {
	key: string;
	label: string;
	isUnknown: boolean;
	measures: ActivityMeasures;
	sharePercent: number;
};

export type ActivityComparisonSummary = {
	mode: ActivityComparisonMode;
	startMs: number;
	endMs: number;
	totals: ActivityMeasures;
	eventCountDelta: number;
	eventCountDeltaPercent: number | null;
	completeness: ActivityCompletenessState;
	retentionPartial: boolean;
	sourceHealthGap: boolean;
};

export type ActivityRetentionEvidence = {
	partialRetention: boolean;
	oldestRetainedEventMs: number | null;
	deletedEventCountInRange: number;
	deletedIntervals: Array<{ startMs: number; endMs: number }>;
};

export type ActivityCameraSourceHealth = {
	cameraId: string;
	displayName: string;
	status: ActivityCameraHealthStatus;
	reportedEvents: number;
	missingHealthMs: number;
	healthyObservedMs: number;
};

export type ActivitySourceHealthSummary = {
	authorizedCameras: number;
	reportingCameras: number;
	healthyNoReportsCameras: number;
	missingHealthCameras: number;
	noExpectedSourceCameras: number;
	unknownHistoryCameras: number;
	cameras: ActivityCameraSourceHealth[];
};

export type ActivitySummaryData = {
	queryId: string;
	generatedAtMs: number;
	startMs: number;
	endMs: number;
	timezone: string;
	tzdbVersion: string;
	granularity: ActivityBucketGranularity;
	weekStart: ActivityWeekStart;
	analyticsGeneration: number;
	groupPolicyRevision: number;
	dataSource: string;
	rollupState: ActivityRollupState;
	totals: ActivityMeasures;
	comparison: ActivityComparisonSummary | null;
	buckets: ActivityTimeBucket[];
	series: ActivitySeries[];
	seriesTruncated: boolean;
	heatmap: ActivityHeatmapCell[];
	byCamera: ActivityBreakdownRow[];
	byEventType: ActivityBreakdownRow[];
	byZone: ActivityBreakdownRow[];
	byOrigin: ActivityBreakdownRow[];
	byConfidenceBand: ActivityBreakdownRow[];
	byGroup: ActivityBreakdownRow[];
	retention: ActivityRetentionEvidence;
	sourceHealth: ActivitySourceHealthSummary;
	provenanceNotice: string;
	exact: boolean;
};

export type ActivityAnalyticsStatusData = {
	state: ActivityRollupState;
	analyticsGeneration: number;
	schemaVersion: number;
	siteTimezone: string;
	weekStart: ActivityWeekStart;
	tzdbVersion: string;
	rollupRowCount: number;
	maxRollupRows: number;
	oldestRetainedEventMs: number | null;
	lastRebuildCompletedAtMs: number | null;
	statusDetail: string | null;
};

export type EventsDrilldownOverride = {
	startMs?: number;
	endMs?: number;
	cameraId?: string;
	cameraIds?: readonly string[];
	type?: string;
	source?: RecordingEvent['source'];
	zone?: string;
	missingZone?: boolean;
	confidenceBand?: EventConfidenceBand;
};

export type EventsDrilldownResult = { ok: true; href: string } | { ok: false; reason: string };

/**
 * Returns true when the given IANA timezone aligns to whole UTC hours at both bounds.
 */
export function isWholeHourUtcOffset(timezone: string, startMs: number, endMs: number): boolean {
	try {
		const fmt = new Intl.DateTimeFormat('en-US', {
			timeZone: timezone,
			hour: '2-digit',
			minute: '2-digit',
			hourCycle: 'h23'
		});
		const minuteAt = (ms: number): number => {
			const aligned = Math.floor(ms / 3_600_000) * 3_600_000;
			const part = fmt.formatToParts(new Date(aligned)).find((item) => item.type === 'minute');
			return part ? Number(part.value) : -1;
		};
		return minuteAt(startMs) === 0 && minuteAt(endMs) === 0;
	} catch {
		return false;
	}
}

/**
 * Builds an exact server-filtered `/events` link for one aggregate bucket or breakdown row.
 * Refuses links that would lose predicates or exceed the bounded 31-day metadata window.
 */
export function buildEventsDrilldownHref(
	summary: Pick<ActivitySummaryData, 'startMs' | 'endMs'>,
	filter: ActivitySummaryFilterOptions,
	override: EventsDrilldownOverride,
	eventsPath = '/events'
): EventsDrilldownResult {
	const startMs = override.startMs ?? summary.startMs;
	const endMs = override.endMs ?? summary.endMs;
	if (!Number.isFinite(startMs) || !Number.isFinite(endMs) || startMs >= endMs) {
		return { ok: false, reason: 'Time interval is invalid.' };
	}
	if (endMs - startMs > MAX_EVENTS_DRILLDOWN_WINDOW_MS) {
		return {
			ok: false,
			reason: 'Select a bucket or window of 31 days or less to open matching events.'
		};
	}
	if ((filter.eventTypes?.length ?? 0) > 1 && !override.type) {
		return { ok: false, reason: 'Multiple event types cannot be opened in one Events view.' };
	}
	if ((filter.origins?.length ?? 0) > 1 && !override.source) {
		return { ok: false, reason: 'Multiple origins cannot be opened in one Events view.' };
	}
	if ((filter.zones?.length ?? 0) > 1 && !override.zone && !override.missingZone) {
		return { ok: false, reason: 'Multiple zones cannot be opened in one Events view.' };
	}
	if ((filter.confidenceBands?.length ?? 0) > 1 && !override.confidenceBand) {
		return { ok: false, reason: 'Multiple confidence bands cannot be opened in one Events view.' };
	}

	if (override.cameraIds !== undefined && override.cameraIds.length === 0) {
		return { ok: false, reason: 'No authorized cameras are currently assigned to this group.' };
	}

	const params = new URLSearchParams();
	params.set('date', new Date(startMs).toISOString().slice(0, 10));
	params.set('startMs', String(Math.trunc(startMs)));
	params.set('endMs', String(Math.trunc(endMs)));

	const cameraIds = override.cameraId
		? [override.cameraId]
		: override.cameraIds !== undefined
			? [...override.cameraIds]
			: filter.sourceIds && filter.sourceIds.length > 0
				? [...filter.sourceIds]
				: [];
	if (cameraIds.length === 1) {
		params.set('camera', cameraIds[0]!);
	} else if (cameraIds.length > 1) {
		params.set('cameras', cameraIds.join(','));
	}

	const eventType = override.type ?? filter.eventTypes?.[0] ?? null;
	if (eventType) params.set('type', eventType);

	const origin = override.source ?? filter.origins?.[0] ?? null;
	if (origin) params.set('source', origin);

	const missingZone = override.missingZone ?? filter.includeMissingZone ?? false;
	const zone = override.missingZone ? null : (override.zone ?? filter.zones?.[0] ?? null);
	if (zone) params.set('zone', zone);
	if (missingZone) params.set('missingZone', '1');

	const band = override.confidenceBand ?? filter.confidenceBands?.[0] ?? null;
	if (band) params.set('confidenceBand', band);

	if (filter.image && filter.image !== 'all') params.set('image', filter.image);
	if (filter.review && filter.review !== 'all') params.set('review', filter.review);
	if (filter.bookmarks && filter.bookmarks !== 'all') params.set('bookmarks', filter.bookmarks);

	return { ok: true, href: `${eventsPath}?${params.toString()}` };
}

export function isEventConfidenceBand(value: string | null): value is EventConfidenceBand {
	return value === 'unknown' || value === 'low' || value === 'medium' || value === 'high';
}

export function formatConfidenceBandLabel(band: EventConfidenceBand): string {
	switch (band) {
		case 'unknown':
			return 'Unknown confidence';
		case 'low':
			return 'Low (0.00-0.49)';
		case 'medium':
			return 'Medium (0.50-0.79)';
		case 'high':
			return 'High (0.80-1.00)';
	}
}

export function formatCompletenessLabel(state: ActivityCompletenessState): string {
	switch (state) {
		case 'complete':
			return 'Complete';
		case 'partial_current':
			return 'Partial (in progress)';
		case 'partial_retention':
			return 'Partial (retention deleted)';
		case 'source_gap':
			return 'Source health gap';
		case 'unknown':
			return 'Unknown history';
	}
}

export function formatDurationMs(durationMs: number | null): string {
	if (durationMs === null || !Number.isFinite(durationMs)) return 'N/A';
	if (durationMs < 1_000) return `${Math.round(durationMs)} ms`;
	const seconds = durationMs / 1_000;
	if (seconds < 60) return `${seconds.toFixed(1)} s`;
	const minutes = seconds / 60;
	if (minutes < 60) return `${minutes.toFixed(1)} min`;
	return `${(minutes / 60).toFixed(1)} h`;
}

/**
 * Neutralizes spreadsheet formula prefixes and formats one RFC 4180 CSV cell.
 */
export function escapeCsvCell(value: string | number | boolean | null | undefined): string {
	if (value === null || value === undefined) return '';
	const raw = String(value);
	const first = raw[0];
	const safe =
		first === '=' ||
		first === '+' ||
		first === '-' ||
		first === '@' ||
		first === '\t' ||
		first === '\r'
			? `'${raw}`
			: raw;
	if (/[",\r\n]/.test(safe)) {
		return `"${safe.replaceAll('"', '""')}"`;
	}
	return safe;
}

/**
 * Serializes the displayed bounded server summary into CSV with UTC and timezone provenance.
 */
export function exportActivitySummaryCsv(summary: ActivitySummaryData): string {
	const lines: string[] = [];
	const pushRow = (cells: Array<string | number | boolean | null | undefined>) => {
		if (lines.length >= MAX_ACTIVITY_EXPORT_ROWS) {
			throw new Error(`Activity CSV export exceeded ${MAX_ACTIVITY_EXPORT_ROWS} rows.`);
		}
		lines.push(cells.map(escapeCsvCell).join(','));
	};

	pushRow(['# keeppeek_activity_summary_v1']);
	pushRow(['# provenance', summary.provenanceNotice]);
	pushRow(['# utc_start', new Date(summary.startMs).toISOString()]);
	pushRow(['# utc_end', new Date(summary.endMs).toISOString()]);
	pushRow(['# timezone', summary.timezone]);
	pushRow(['# tzdb_version', summary.tzdbVersion]);
	pushRow(['# granularity', summary.granularity]);
	pushRow(['# week_start', summary.weekStart]);
	pushRow(['# analytics_generation', summary.analyticsGeneration]);
	pushRow(['# group_policy_revision', summary.groupPolicyRevision]);
	pushRow(['# data_source', summary.dataSource]);
	pushRow(['# rollup_state', summary.rollupState]);
	pushRow(['# partial_retention', summary.retention.partialRetention]);
	pushRow(['# missing_health_cameras', summary.sourceHealth.missingHealthCameras]);
	pushRow([
		'section',
		'key_or_label',
		'utc_start',
		'utc_end',
		'utc_offset_seconds',
		'dst_fold',
		'completeness',
		'event_count',
		'distinct_event_count',
		'interval_event_count',
		'open_interval_event_count',
		'average_duration_ms',
		'percent_with_image',
		'percent_with_confidence',
		'percent_with_zone',
		'share_percent'
	]);

	for (const bucket of summary.buckets) {
		pushRow([
			'bucket',
			bucket.label,
			new Date(bucket.startMs).toISOString(),
			new Date(bucket.endMs).toISOString(),
			bucket.utcOffsetSeconds,
			bucket.dstFold,
			bucket.completeness,
			bucket.measures.eventCount,
			bucket.measures.distinctEventCount,
			bucket.measures.intervalEventCount,
			bucket.measures.openIntervalEventCount,
			bucket.measures.averageDurationMs,
			bucket.measures.percentWithImage,
			bucket.measures.percentWithConfidence,
			bucket.measures.percentWithZone,
			null
		]);
	}

	const sections: Array<[string, readonly ActivityBreakdownRow[]]> = [
		['camera', summary.byCamera],
		['event_type', summary.byEventType],
		['zone', summary.byZone],
		['origin', summary.byOrigin],
		['confidence_band', summary.byConfidenceBand],
		['group', summary.byGroup]
	];
	for (const [section, rows] of sections) {
		for (const row of rows) {
			pushRow([
				section,
				row.isUnknown ? `${row.label} (unknown)` : row.label,
				new Date(summary.startMs).toISOString(),
				new Date(summary.endMs).toISOString(),
				null,
				null,
				summary.retention.partialRetention ? 'partial_retention' : 'complete',
				row.measures.eventCount,
				row.measures.distinctEventCount,
				row.measures.intervalEventCount,
				row.measures.openIntervalEventCount,
				row.measures.averageDurationMs,
				row.measures.percentWithImage,
				row.measures.percentWithConfidence,
				row.measures.percentWithZone,
				row.sharePercent
			]);
		}
	}

	const csv = `${lines.join('\r\n')}\r\n`;
	if (new TextEncoder().encode(csv).byteLength > MAX_ACTIVITY_EXPORT_BYTES) {
		throw new Error(`Activity CSV export exceeded ${MAX_ACTIVITY_EXPORT_BYTES} bytes.`);
	}
	return csv;
}

/**
 * Serializes the displayed bounded server summary into JSON with explicit UTC and provenance metadata.
 */
export function exportActivitySummaryJson(summary: ActivitySummaryData): string {
	const payload = {
		schema: 'keeppeek.activity-summary.export.v1',
		exportedAtUtc: new Date(summary.generatedAtMs).toISOString(),
		utcRange: {
			start: new Date(summary.startMs).toISOString(),
			end: new Date(summary.endMs).toISOString()
		},
		timezone: summary.timezone,
		tzdbVersion: summary.tzdbVersion,
		weekStart: summary.weekStart,
		granularity: summary.granularity,
		analyticsGeneration: summary.analyticsGeneration,
		groupPolicyRevision: summary.groupPolicyRevision,
		dataSource: summary.dataSource,
		rollupState: summary.rollupState,
		provenanceNotice: summary.provenanceNotice,
		exact: summary.exact,
		totals: summary.totals,
		comparison: summary.comparison,
		retention: summary.retention,
		sourceHealth: summary.sourceHealth,
		buckets: summary.buckets,
		series: summary.series,
		seriesTruncated: summary.seriesTruncated,
		heatmap: summary.heatmap,
		breakdowns: {
			byCamera: summary.byCamera,
			byEventType: summary.byEventType,
			byZone: summary.byZone,
			byOrigin: summary.byOrigin,
			byConfidenceBand: summary.byConfidenceBand,
			byGroup: summary.byGroup
		}
	};
	const json = JSON.stringify(payload, null, 2);
	if (new TextEncoder().encode(json).byteLength > MAX_ACTIVITY_EXPORT_BYTES) {
		throw new Error(`Activity JSON export exceeded ${MAX_ACTIVITY_EXPORT_BYTES} bytes.`);
	}
	return json;
}
