import { create, fromBinary } from '@bufbuild/protobuf';
import { timestampDate, timestampFromDate, type Timestamp } from '@bufbuild/protobuf/wkt';
import {
	ActivityBucketGranularity as ProtoGranularity,
	ActivityCameraHealthStatus as ProtoCameraHealthStatus,
	ActivityComparisonMode as ProtoComparisonMode,
	ActivityCompletenessState as ProtoCompletenessState,
	ActivityDimension as ProtoDimension,
	ActivityRollupState as ProtoRollupState,
	ActivitySummaryFilterSchema,
	ActivitySummaryResponseSchema,
	ActivityWeekStart as ProtoWeekStart,
	DataChannelKind,
	EventConfidenceBand as ProtoConfidenceBand,
	EventImageFilter as ProtoImageFilter,
	EventOrigin,
	QueryActivitySummarySchema,
	type ActivityAnalyticsChunk,
	type ActivityAnalyticsStatus,
	type ActivityBreakdownRow as ProtoBreakdownRow,
	type ActivityMeasures as ProtoMeasures,
	type ActivitySummaryResponse,
	type EventWorkflowQuery,
	type QueryActivitySummary
} from './proto/webrtc_pb';
import type {
	ActivityAnalyticsStatusData,
	ActivityBreakdownRow,
	ActivityBucketGranularity,
	ActivityCameraHealthStatus,
	ActivityComparisonMode,
	ActivityCompletenessState,
	ActivityDimension,
	ActivityMeasures,
	ActivityRollupState,
	ActivitySummaryData,
	ActivitySummaryQueryOptions,
	ActivityWeekStart,
	EventConfidenceBand
} from './activity-analytics';

export const MAX_ANALYTICS_RESPONSE_BYTES = 1_048_576;

export type PendingAnalyticsTransfer = {
	chunks: Map<number, Uint8Array>;
	chunkCount: number | null;
	totalBytes: number;
	resolve: (data: ActivitySummaryData) => void;
	reject: (error: unknown) => void;
};

export function protoConfidenceBand(band: EventConfidenceBand): ProtoConfidenceBand {
	switch (band) {
		case 'unknown':
			return ProtoConfidenceBand.UNKNOWN;
		case 'low':
			return ProtoConfidenceBand.LOW;
		case 'medium':
			return ProtoConfidenceBand.MEDIUM;
		case 'high':
			return ProtoConfidenceBand.HIGH;
	}
}

export function buildProtoActivityQuery(
	queryId: string,
	options: ActivitySummaryQueryOptions,
	workflowQuery?: EventWorkflowQuery
): QueryActivitySummary {
	const filter = options.filter ?? {};
	const image =
		filter.image === 'with'
			? ProtoImageFilter.WITH_IMAGE
			: filter.image === 'without'
				? ProtoImageFilter.WITHOUT_IMAGE
				: ProtoImageFilter.ANY;
	return create(QueryActivitySummarySchema, {
		queryId,
		startTime: timestampFromDate(new Date(options.startMs)),
		endTime: timestampFromDate(new Date(options.endMs)),
		timezone: options.timezone ?? '',
		granularity: protoGranularity(options.granularity),
		weekStart: options.weekStart ? protoWeekStart(options.weekStart) : ProtoWeekStart.UNSPECIFIED,
		filter: create(ActivitySummaryFilterSchema, {
			sourceIds: [...(filter.sourceIds ?? [])],
			groupIds: [...(filter.groupIds ?? [])],
			eventTypes: [...(filter.eventTypes ?? [])],
			origins: (filter.origins ?? []).map((origin) =>
				origin === 'keeppeek' ? EventOrigin.KEEPPEEK : EventOrigin.CAMERA
			),
			zones: [...(filter.zones ?? [])],
			includeMissingZone: filter.includeMissingZone ?? false,
			confidenceBands: (filter.confidenceBands ?? []).map(protoConfidenceBand),
			image,
			workflow: workflowQuery
		}),
		groupBy: (options.groupBy ?? []).map(protoDimension),
		comparison: protoComparisonMode(options.comparison ?? 'none'),
		denseBuckets: options.denseBuckets ?? true,
		maxBuckets: options.maxBuckets ?? 366,
		maxSeries: options.maxSeries ?? 64,
		maxRankedRows: options.maxRankedRows ?? 64,
		includeHeatmap: options.includeHeatmap ?? false,
		channel: DataChannelKind.RELIABLE_DATA
	});
}

export function decodeAnalyticsStatus(
	status: ActivityAnalyticsStatus
): ActivityAnalyticsStatusData {
	return {
		state: fromProtoRollupState(status.state),
		analyticsGeneration: Number(status.analyticsGeneration),
		schemaVersion: status.schemaVersion,
		siteTimezone: status.siteTimezone || 'UTC',
		weekStart: status.weekStart === ProtoWeekStart.SUNDAY ? 'sunday' : 'monday',
		tzdbVersion: status.tzdbVersion,
		rollupRowCount: Number(status.rollupRowCount),
		maxRollupRows: Number(status.maxRollupRows),
		oldestRetainedEventMs: tsMs(status.oldestRetainedEventTime),
		lastRebuildCompletedAtMs: tsMs(status.lastRebuildCompletedAt),
		statusDetail: status.statusDetail ?? null
	};
}

export function appendAnalyticsChunk(
	pending: PendingAnalyticsTransfer,
	chunk: ActivityAnalyticsChunk
): ActivitySummaryData | null {
	if (chunk.chunkCount === 0 || chunk.chunkIndex >= chunk.chunkCount) {
		throw new Error('Activity analytics chunk metadata is invalid.');
	}
	if (pending.chunkCount === null) {
		pending.chunkCount = chunk.chunkCount;
	} else if (pending.chunkCount !== chunk.chunkCount) {
		throw new Error('Activity analytics chunk count changed during transfer.');
	}
	if (pending.chunks.has(chunk.chunkIndex)) {
		throw new Error('Duplicate activity analytics chunk received.');
	}
	pending.totalBytes += chunk.payload.byteLength;
	if (pending.totalBytes > MAX_ANALYTICS_RESPONSE_BYTES) {
		throw new Error('Activity analytics response exceeded 1 MiB limit.');
	}
	pending.chunks.set(chunk.chunkIndex, chunk.payload);
	if (pending.chunks.size < pending.chunkCount) {
		return null;
	}
	const combined = new Uint8Array(pending.totalBytes);
	let offset = 0;
	for (let index = 0; index < pending.chunkCount; index++) {
		const part = pending.chunks.get(index);
		if (!part) throw new Error('Activity analytics response missed a chunk.');
		combined.set(part, offset);
		offset += part.byteLength;
	}
	const response = fromBinary(ActivitySummaryResponseSchema, combined);
	return decodeActivitySummaryResponse(response);
}

export function decodeActivitySummaryResponse(
	response: ActivitySummaryResponse
): ActivitySummaryData {
	return {
		queryId: response.queryId,
		generatedAtMs: tsMs(response.generatedAt) ?? Date.now(),
		startMs: tsMs(response.startTime) ?? 0,
		endMs: tsMs(response.endTime) ?? 0,
		timezone: response.timezone || 'UTC',
		tzdbVersion: response.tzdbVersion,
		granularity: fromProtoGranularity(response.granularity),
		weekStart: response.weekStart === ProtoWeekStart.SUNDAY ? 'sunday' : 'monday',
		analyticsGeneration: Number(response.analyticsGeneration),
		groupPolicyRevision: Number(response.groupPolicyRevision),
		dataSource: response.dataSource,
		rollupState: fromProtoRollupState(response.rollupState),
		totals: decodeMeasures(response.totals),
		comparison: response.comparison
			? {
					mode: fromProtoComparisonMode(response.comparison.mode),
					startMs: tsMs(response.comparison.startTime) ?? 0,
					endMs: tsMs(response.comparison.endTime) ?? 0,
					totals: decodeMeasures(response.comparison.totals),
					eventCountDelta: Number(response.comparison.eventCountDelta),
					eventCountDeltaPercent: response.comparison.eventCountDeltaPercent ?? null,
					completeness: fromProtoCompleteness(response.comparison.completeness),
					retentionPartial: response.comparison.retentionPartial,
					sourceHealthGap: response.comparison.sourceHealthGap
				}
			: null,
		buckets: response.buckets.map((bucket) => ({
			startMs: tsMs(bucket.startTime) ?? 0,
			endMs: tsMs(bucket.endTime) ?? 0,
			label: bucket.label,
			utcOffsetSeconds: bucket.utcOffsetSeconds,
			dstFold: bucket.dstFold,
			partialCurrent: bucket.partialCurrent,
			completeness: fromProtoCompleteness(bucket.completeness),
			retentionPartial: bucket.retentionPartial,
			sourceHealthGap: bucket.sourceHealthGap,
			measures: decodeMeasures(bucket.measures)
		})),
		series: response.series.map((series) => {
			const dimensions: Partial<Record<ActivityDimension, string>> = {};
			for (const entry of series.dimensions) {
				const dim = fromProtoDimension(entry.dimension);
				if (dim) dimensions[dim] = entry.value;
			}
			return {
				dimensions,
				label: series.label,
				totals: decodeMeasures(series.totals),
				bucketCounts: series.bucketCounts.map(Number)
			};
		}),
		seriesTruncated: response.seriesTruncated,
		heatmap: response.heatmap.map((cell) => ({
			weekday: cell.weekday,
			localHour: cell.localHour,
			eventCount: Number(cell.eventCount),
			distinctEventCount: Number(cell.distinctEventCount),
			completeness: fromProtoCompleteness(cell.completeness)
		})),
		byCamera: response.byCamera.map(decodeBreakdown),
		byEventType: response.byEventType.map(decodeBreakdown),
		byZone: response.byZone.map(decodeBreakdown),
		byOrigin: response.byOrigin.map(decodeBreakdown),
		byConfidenceBand: response.byConfidenceBand.map(decodeBreakdown),
		byGroup: response.byGroup.map(decodeBreakdown),
		retention: {
			partialRetention: response.retention?.partialRetention ?? false,
			oldestRetainedEventMs: tsMs(response.retention?.oldestRetainedEventTime),
			deletedEventCountInRange: Number(response.retention?.deletedEventCountInRange ?? 0n),
			deletedIntervals: (response.retention?.deletedIntervals ?? []).map((interval) => ({
				startMs: tsMs(interval.startTime) ?? 0,
				endMs: tsMs(interval.endTime) ?? 0
			}))
		},
		sourceHealth: {
			authorizedCameras: Number(response.sourceHealth?.authorizedCameras ?? 0n),
			reportingCameras: Number(response.sourceHealth?.reportingCameras ?? 0n),
			healthyNoReportsCameras: Number(response.sourceHealth?.healthyNoReportsCameras ?? 0n),
			missingHealthCameras: Number(response.sourceHealth?.missingHealthCameras ?? 0n),
			noExpectedSourceCameras: Number(response.sourceHealth?.noExpectedSourceCameras ?? 0n),
			unknownHistoryCameras: Number(response.sourceHealth?.unknownHistoryCameras ?? 0n),
			cameras: (response.sourceHealth?.cameras ?? []).map((cam) => ({
				cameraId: cam.cameraId,
				displayName: cam.displayName,
				status: fromProtoCameraHealthStatus(cam.status),
				reportedEvents: Number(cam.reportedEvents),
				missingHealthMs: Number(cam.missingHealthMs),
				healthyObservedMs: Number(cam.healthyObservedMs)
			}))
		},
		provenanceNotice: response.provenanceNotice,
		exact: response.exact
	};
}

function decodeMeasures(measures?: ProtoMeasures): ActivityMeasures {
	return {
		eventCount: Number(measures?.eventCount ?? 0n),
		distinctEventCount: Number(measures?.distinctEventCount ?? 0n),
		intervalEventCount: Number(measures?.intervalEventCount ?? 0n),
		openIntervalEventCount: Number(measures?.openIntervalEventCount ?? 0n),
		totalDurationMs: Number(measures?.totalDurationMs ?? 0n),
		averageDurationMs: measures?.averageDurationMs ?? null,
		withImageCount: Number(measures?.withImageCount ?? 0n),
		withConfidenceCount: Number(measures?.withConfidenceCount ?? 0n),
		withZoneCount: Number(measures?.withZoneCount ?? 0n),
		percentWithImage: measures?.percentWithImage ?? null,
		percentWithConfidence: measures?.percentWithConfidence ?? null,
		percentWithZone: measures?.percentWithZone ?? null
	};
}

function decodeBreakdown(row: ProtoBreakdownRow): ActivityBreakdownRow {
	return {
		key: row.key,
		label: row.label,
		isUnknown: row.isUnknown,
		measures: decodeMeasures(row.measures),
		sharePercent: row.sharePercent
	};
}

function tsMs(timestamp?: Timestamp): number | null {
	return timestamp ? timestampDate(timestamp).getTime() : null;
}

function protoGranularity(value: ActivityBucketGranularity): ProtoGranularity {
	switch (value) {
		case 'hour':
			return ProtoGranularity.HOUR;
		case 'day':
			return ProtoGranularity.DAY;
		case 'week':
			return ProtoGranularity.WEEK;
		case 'month':
			return ProtoGranularity.MONTH;
		case 'hour_of_day':
			return ProtoGranularity.HOUR_OF_DAY;
		case 'weekday_weekend':
			return ProtoGranularity.WEEKDAY_WEEKEND;
	}
}

function fromProtoGranularity(value: ProtoGranularity): ActivityBucketGranularity {
	switch (value) {
		case ProtoGranularity.HOUR:
			return 'hour';
		case ProtoGranularity.WEEK:
			return 'week';
		case ProtoGranularity.MONTH:
			return 'month';
		case ProtoGranularity.HOUR_OF_DAY:
			return 'hour_of_day';
		case ProtoGranularity.WEEKDAY_WEEKEND:
			return 'weekday_weekend';
		default:
			return 'day';
	}
}

function protoWeekStart(value: ActivityWeekStart): ProtoWeekStart {
	return value === 'sunday' ? ProtoWeekStart.SUNDAY : ProtoWeekStart.MONDAY;
}

function protoDimension(value: ActivityDimension): ProtoDimension {
	switch (value) {
		case 'camera':
			return ProtoDimension.CAMERA;
		case 'group':
			return ProtoDimension.GROUP;
		case 'event_type':
			return ProtoDimension.EVENT_TYPE;
		case 'origin':
			return ProtoDimension.ORIGIN;
		case 'zone':
			return ProtoDimension.ZONE;
		case 'confidence_band':
			return ProtoDimension.CONFIDENCE_BAND;
		case 'image':
			return ProtoDimension.IMAGE;
		case 'workflow':
			return ProtoDimension.WORKFLOW;
	}
}

function fromProtoDimension(value: ProtoDimension): ActivityDimension | null {
	switch (value) {
		case ProtoDimension.CAMERA:
			return 'camera';
		case ProtoDimension.GROUP:
			return 'group';
		case ProtoDimension.EVENT_TYPE:
			return 'event_type';
		case ProtoDimension.ORIGIN:
			return 'origin';
		case ProtoDimension.ZONE:
			return 'zone';
		case ProtoDimension.CONFIDENCE_BAND:
			return 'confidence_band';
		case ProtoDimension.IMAGE:
			return 'image';
		case ProtoDimension.WORKFLOW:
			return 'workflow';
		default:
			return null;
	}
}

function protoComparisonMode(value: ActivityComparisonMode): ProtoComparisonMode {
	switch (value) {
		case 'previous_duration':
			return ProtoComparisonMode.PREVIOUS_DURATION;
		case 'previous_calendar':
			return ProtoComparisonMode.PREVIOUS_CALENDAR;
		default:
			return ProtoComparisonMode.NONE;
	}
}

function fromProtoComparisonMode(value: ProtoComparisonMode): ActivityComparisonMode {
	switch (value) {
		case ProtoComparisonMode.PREVIOUS_DURATION:
			return 'previous_duration';
		case ProtoComparisonMode.PREVIOUS_CALENDAR:
			return 'previous_calendar';
		default:
			return 'none';
	}
}

function fromProtoRollupState(value: ProtoRollupState): ActivityRollupState {
	switch (value) {
		case ProtoRollupState.READY:
			return 'ready';
		case ProtoRollupState.REBUILDING:
			return 'rebuilding';
		case ProtoRollupState.DEGRADED:
			return 'degraded';
		default:
			return 'unavailable';
	}
}

function fromProtoCompleteness(value: ProtoCompletenessState): ActivityCompletenessState {
	switch (value) {
		case ProtoCompletenessState.COMPLETE:
			return 'complete';
		case ProtoCompletenessState.PARTIAL_CURRENT:
			return 'partial_current';
		case ProtoCompletenessState.PARTIAL_RETENTION:
			return 'partial_retention';
		case ProtoCompletenessState.SOURCE_GAP:
			return 'source_gap';
		default:
			return 'unknown';
	}
}

function fromProtoCameraHealthStatus(value: ProtoCameraHealthStatus): ActivityCameraHealthStatus {
	switch (value) {
		case ProtoCameraHealthStatus.REPORTING:
			return 'reporting';
		case ProtoCameraHealthStatus.HEALTHY_NO_REPORTS:
			return 'healthy_no_reports';
		case ProtoCameraHealthStatus.MISSING_HEALTH:
			return 'missing_health';
		case ProtoCameraHealthStatus.NO_EXPECTED_SOURCE:
			return 'no_expected_source';
		default:
			return 'unknown_history';
	}
}
