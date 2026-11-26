import { describe, expect, it } from 'vitest';
import { create, toBinary } from '@bufbuild/protobuf';
import { timestampFromDate } from '@bufbuild/protobuf/wkt';
import {
	ActivityAnalyticsChunkSchema,
	ActivityBucketGranularity,
	ActivityCompletenessState,
	ActivityRollupState,
	ActivitySummaryResponseSchema,
	ActivityWeekStart
} from './proto/webrtc_pb';
import {
	buildEventsDrilldownHref,
	escapeCsvCell,
	exportActivitySummaryCsv,
	exportActivitySummaryJson,
	isWholeHourUtcOffset,
	type ActivitySummaryData
} from './activity-analytics';
import {
	appendAnalyticsChunk,
	buildProtoActivityQuery,
	type PendingAnalyticsTransfer
} from './control-client-analytics';
import {
	eventBrowserQueryBounds,
	eventFilterSummary,
	parseEventBrowserFilters
} from './event-browser';

function sampleSummary(): ActivitySummaryData {
	const startMs = Date.parse('2026-05-10T10:00:00Z');
	const endMs = Date.parse('2026-05-10T12:00:00Z');
	return {
		queryId: 'q-1',
		generatedAtMs: Date.parse('2026-05-10T12:05:00Z'),
		startMs,
		endMs,
		timezone: 'America/New_York',
		tzdbVersion: '2025b',
		granularity: 'hour',
		weekStart: 'monday',
		analyticsGeneration: 42,
		groupPolicyRevision: 3,
		dataSource: 'rollups',
		rollupState: 'ready',
		totals: {
			eventCount: 5,
			distinctEventCount: 5,
			intervalEventCount: 4,
			openIntervalEventCount: 1,
			totalDurationMs: 120_000,
			averageDurationMs: 30_000,
			withImageCount: 3,
			withConfidenceCount: 4,
			withZoneCount: 4,
			percentWithImage: 60,
			percentWithConfidence: 80,
			percentWithZone: 80
		},
		comparison: null,
		buckets: [
			{
				startMs,
				endMs: startMs + 3_600_000,
				label: '2026-05-10 06:00 -04:00',
				utcOffsetSeconds: -14_400,
				dstFold: 0,
				partialCurrent: false,
				completeness: 'partial_retention',
				retentionPartial: true,
				sourceHealthGap: false,
				measures: {
					eventCount: 3,
					distinctEventCount: 3,
					intervalEventCount: 3,
					openIntervalEventCount: 0,
					totalDurationMs: 90_000,
					averageDurationMs: 30_000,
					withImageCount: 2,
					withConfidenceCount: 3,
					withZoneCount: 2,
					percentWithImage: 66.7,
					percentWithConfidence: 100,
					percentWithZone: 66.7
				}
			}
		],
		series: [],
		seriesTruncated: false,
		heatmap: [],
		byCamera: [
			{
				key: 'cam-front',
				label: '=SUM(A1:A2)',
				isUnknown: false,
				measures: {
					eventCount: 5,
					distinctEventCount: 5,
					intervalEventCount: 4,
					openIntervalEventCount: 1,
					totalDurationMs: 120_000,
					averageDurationMs: 30_000,
					withImageCount: 3,
					withConfidenceCount: 4,
					withZoneCount: 4,
					percentWithImage: 60,
					percentWithConfidence: 80,
					percentWithZone: 80
				},
				sharePercent: 100
			}
		],
		byEventType: [],
		byZone: [
			{
				key: '',
				label: 'Unknown zone',
				isUnknown: true,
				measures: {
					eventCount: 1,
					distinctEventCount: 1,
					intervalEventCount: 1,
					openIntervalEventCount: 0,
					totalDurationMs: 10_000,
					averageDurationMs: 10_000,
					withImageCount: 0,
					withConfidenceCount: 0,
					withZoneCount: 0,
					percentWithImage: 0,
					percentWithConfidence: 0,
					percentWithZone: 0
				},
				sharePercent: 20
			}
		],
		byOrigin: [],
		byConfidenceBand: [],
		byGroup: [],
		retention: {
			partialRetention: true,
			oldestRetainedEventMs: startMs,
			deletedEventCountInRange: 2,
			deletedIntervals: [{ startMs, endMs: startMs + 60_000 }]
		},
		sourceHealth: {
			authorizedCameras: 2,
			reportingCameras: 1,
			healthyNoReportsCameras: 0,
			missingHealthCameras: 1,
			noExpectedSourceCameras: 0,
			unknownHistoryCameras: 0,
			cameras: []
		},
		provenanceNotice:
			'Counts reflect producer-reported event metadata within current retention and authorization scope, not authoritative real-world identity, occupancy, or threat totals.',
		exact: true
	};
}

describe('activity-analytics helpers', () => {
	it('builds exact Events drilldown URLs and round-trips through parseEventBrowserFilters', () => {
		const summary = sampleSummary();
		const link = buildEventsDrilldownHref(
			summary,
			{ image: 'with', review: 'reviewed', bookmarks: 'mine' },
			{
				startMs: summary.buckets[0]!.startMs,
				endMs: summary.buckets[0]!.endMs,
				cameraIds: ['cam-front', 'cam-side'],
				missingZone: true,
				confidenceBand: 'medium'
			}
		);
		expect(link.ok).toBe(true);
		if (!link.ok) return;
		const parsedUrl = new URL(link.href, 'http://localhost');
		const filters = parseEventBrowserFilters(parsedUrl.searchParams, '2026-01-01');
		expect(filters.cameraIds).toEqual(['cam-front', 'cam-side']);
		expect(filters.missingZone).toBe(true);
		expect(filters.confidenceBand).toBe('medium');
		expect(filters.image).toBe('with');
		expect(filters.review).toBe('reviewed');
		expect(filters.bookmarks).toBe('mine');
		expect(eventBrowserQueryBounds(filters)).toEqual({
			startMs: summary.buckets[0]!.startMs,
			endMs: summary.buckets[0]!.endMs
		});
	});

	it('refuses drilldown links that exceed 31 days or would lose multi-value predicates', () => {
		const summary = sampleSummary();
		const longLink = buildEventsDrilldownHref(
			{ startMs: Date.parse('2026-01-01T00:00:00Z'), endMs: Date.parse('2026-04-01T00:00:00Z') },
			{},
			{}
		);
		expect(longLink.ok).toBe(false);

		const multiType = buildEventsDrilldownHref(summary, { eventTypes: ['person', 'vehicle'] }, {});
		expect(multiType.ok).toBe(false);

		const emptyGroup = buildEventsDrilldownHref(summary, {}, { cameraIds: [] });
		expect(emptyGroup.ok).toBe(false);
	});

	it('detects sub-hour UTC offset timezones and summarizes drill-down filters', () => {
		const start = Date.parse('2026-06-01T00:00:00Z');
		const end = Date.parse('2026-06-30T00:00:00Z');
		expect(isWholeHourUtcOffset('UTC', start, end)).toBe(true);
		expect(isWholeHourUtcOffset('America/New_York', start, end)).toBe(true);
		expect(isWholeHourUtcOffset('Asia/Kolkata', start, end)).toBe(false);

		const parsed = parseEventBrowserFilters(
			new URLSearchParams({
				date: '2026-06-01',
				startMs: String(start),
				endMs: String(start + 3_600_000),
				cameras: 'cam-1,cam-2',
				missingZone: '1',
				confidenceBand: 'high'
			}),
			'2026-01-01'
		);
		const summaryText = eventFilterSummary(parsed);
		expect(summaryText).toContain('2 cameras');
		expect(summaryText).toContain('missing zone');
		expect(summaryText).toContain('confidence band high');
		expect(summaryText).toContain('2026-06-01T00:00:00.000Z');
	});

	it('neutralizes CSV formula prefixes and exports UTC, timezone, and provenance metadata', () => {
		expect(escapeCsvCell('=SUM(A1:A2)')).toBe("'=SUM(A1:A2)");
		expect(escapeCsvCell('+1+2')).toBe("'+1+2");
		expect(escapeCsvCell('-cmd')).toBe("'-cmd");
		expect(escapeCsvCell('@evil')).toBe("'@evil");

		const summary = sampleSummary();
		const csv = exportActivitySummaryCsv(summary);
		expect(csv).toContain('# timezone,America/New_York');
		expect(csv).toContain('# utc_start,2026-05-10T10:00:00.000Z');
		expect(csv).toContain('# partial_retention,true');
		expect(csv).toContain("'=SUM(A1:A2)");
		expect(csv).toContain('Unknown zone (unknown)');

		const json = JSON.parse(exportActivitySummaryJson(summary));
		expect(json.timezone).toBe('America/New_York');
		expect(json.utcRange.start).toBe('2026-05-10T10:00:00.000Z');
		expect(json.retention.partialRetention).toBe(true);
		expect(json.provenanceNotice).toContain('producer-reported event metadata');
	});

	it('encodes queries and reassembles chunked protobuf responses within 1 MiB', () => {
		const protoQuery = buildProtoActivityQuery('q-proto', {
			startMs: Date.parse('2026-05-10T00:00:00Z'),
			endMs: Date.parse('2026-05-11T00:00:00Z'),
			timezone: 'Asia/Kolkata',
			granularity: 'day',
			weekStart: 'sunday',
			includeHeatmap: true,
			filter: {
				includeMissingZone: true,
				confidenceBands: ['unknown', 'high']
			}
		});
		expect(protoQuery.timezone).toBe('Asia/Kolkata');
		expect(protoQuery.filter?.includeMissingZone).toBe(true);
		expect(protoQuery.filter?.confidenceBands.length).toBe(2);

		const response = create(ActivitySummaryResponseSchema, {
			queryId: 'q-proto',
			generatedAt: timestampFromDate(new Date('2026-05-11T00:00:00Z')),
			startTime: timestampFromDate(new Date('2026-05-10T00:00:00Z')),
			endTime: timestampFromDate(new Date('2026-05-11T00:00:00Z')),
			timezone: 'Asia/Kolkata',
			tzdbVersion: '2025b',
			granularity: ActivityBucketGranularity.DAY,
			weekStart: ActivityWeekStart.SUNDAY,
			analyticsGeneration: 7n,
			groupPolicyRevision: 1n,
			dataSource: 'rollups',
			rollupState: ActivityRollupState.READY,
			provenanceNotice: 'Producer-reported metadata only.',
			exact: true,
			buckets: [
				{
					startTime: timestampFromDate(new Date('2026-05-10T00:00:00Z')),
					endTime: timestampFromDate(new Date('2026-05-11T00:00:00Z')),
					label: '2026-05-10',
					utcOffsetSeconds: 19_800,
					dstFold: 0,
					partialCurrent: false,
					completeness: ActivityCompletenessState.COMPLETE,
					retentionPartial: false,
					sourceHealthGap: false
				}
			]
		});
		const encoded = toBinary(ActivitySummaryResponseSchema, response);
		const mid = Math.ceil(encoded.byteLength / 2);
		const pending: PendingAnalyticsTransfer = {
			chunks: new Map(),
			chunkCount: null,
			totalBytes: 0,
			resolve: () => {},
			reject: () => {}
		};
		const first = appendAnalyticsChunk(
			pending,
			create(ActivityAnalyticsChunkSchema, {
				queryId: 'q-proto',
				chunkIndex: 0,
				chunkCount: 2,
				payload: encoded.slice(0, mid)
			})
		);
		expect(first).toBeNull();
		const completed = appendAnalyticsChunk(
			pending,
			create(ActivityAnalyticsChunkSchema, {
				queryId: 'q-proto',
				chunkIndex: 1,
				chunkCount: 2,
				payload: encoded.slice(mid)
			})
		);
		expect(completed).not.toBeNull();
		expect(completed?.timezone).toBe('Asia/Kolkata');
		expect(completed?.weekStart).toBe('sunday');
		expect(completed?.buckets[0]?.utcOffsetSeconds).toBe(19_800);
	});
});
