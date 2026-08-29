import type { RecordingEvent, RecordingSegment } from './types';
import { buildTimelineAvailability, type TimelineAvailability } from './timeline-availability';

export const keepModes = ['timeline', 'stories', 'swimlanes', 'export'] as const;
export type KeepMode = (typeof keepModes)[number];

export type ExportRange = {
	startMs: number;
	endMs: number;
	durationMs: number;
	estimatedBytes: number | null;
};

export type ExportCandidate = {
	id: string;
	sourceId: string;
	streamId: 'main' | 'sub';
	requestedStartMs: number;
	requestedEndMs: number;
	status: 'running' | 'partial' | 'ready' | 'failed' | 'cancelled' | 'expired';
	expiresAtMs: number | null;
	fileName: string | null;
	sha256: string | null;
	missingRanges: readonly { startMs: number; endMs: number }[];
	burnInTimestamp: boolean;
};

export type ExportDraftIdentity = {
	sourceId: string;
	streamId: 'main' | 'sub';
	startMs: number;
	endMs: number;
	allowPartial: boolean;
	burnInTimestamp: boolean;
};

export type ExportCandidateMatch<T extends ExportCandidate> = {
	exactActive: T | null;
	exactReady: T | null;
	related: T[];
};

export type SwimlaneInput = {
	cameraId: string;
	segments: readonly RecordingSegment[];
};

export type SwimlaneWindow = {
	startMs: number;
	endMs: number;
	lanes: readonly {
		cameraId: string;
		availability: TimelineAvailability;
	}[];
};

export const MAX_EXPORT_DURATION_MS = 2 * 60_000;
export const EVENT_EXPORT_CONTEXT_MS = 15_000;
const SWIMLANE_WINDOW_MS = 60 * 60_000;

export function parseKeepMode(value: string | null): KeepMode {
	return keepModes.includes(value as KeepMode) ? (value as KeepMode) : 'timeline';
}

export function storyEvents(events: readonly RecordingEvent[]): RecordingEvent[] {
	return events
		.filter((event) => event.kind.trim().toLocaleLowerCase() === 'story')
		.toSorted((left, right) => right.start_time_ms - left.start_time_ms);
}

export function createExportRange(
	segment: RecordingSegment | null,
	bitrateKbps: number | null
): ExportRange | null {
	if (segment === null || segment.end_time_ms <= segment.start_time_ms) return null;
	const endMs = Math.min(segment.end_time_ms, segment.start_time_ms + MAX_EXPORT_DURATION_MS);
	const durationMs = endMs - segment.start_time_ms;
	return {
		startMs: segment.start_time_ms,
		endMs,
		durationMs,
		estimatedBytes:
			bitrateKbps === null || bitrateKbps <= 0
				? null
				: Math.round((bitrateKbps * 1_000 * (durationMs / 1_000)) / 8)
	};
}

export function createEventExportRange(
	event: Pick<RecordingEvent, 'start_time_ms' | 'end_time_ms'>,
	bitrateKbps: number | null
): ExportRange {
	const eventEndMs = Math.max(
		event.start_time_ms + 1,
		event.end_time_ms ?? event.start_time_ms + 1
	);
	const startMs = event.start_time_ms - EVENT_EXPORT_CONTEXT_MS;
	const requestedEndMs = eventEndMs + EVENT_EXPORT_CONTEXT_MS;
	const endMs = Math.min(requestedEndMs, startMs + MAX_EXPORT_DURATION_MS);
	const durationMs = endMs - startMs;
	return {
		startMs,
		endMs,
		durationMs,
		estimatedBytes:
			bitrateKbps === null || bitrateKbps <= 0
				? null
				: Math.round((bitrateKbps * 1_000 * (durationMs / 1_000)) / 8)
	};
}

export function classifyExportCandidates<T extends ExportCandidate>(
	jobs: readonly T[],
	draft: ExportDraftIdentity,
	nowMs = Date.now()
): ExportCandidateMatch<T> {
	const candidates = jobs
		.filter(
			(candidate) =>
				candidate.sourceId === draft.sourceId &&
				candidate.streamId === draft.streamId &&
				(candidate.status === 'running' ||
					(candidate.status === 'ready' &&
						(candidate.expiresAtMs === null || candidate.expiresAtMs > nowMs) &&
						candidate.fileName !== null &&
						candidate.sha256 !== null)) &&
				candidate.requestedStartMs < draft.endMs &&
				candidate.requestedEndMs > draft.startMs
		)
		.toSorted((left, right) => right.requestedEndMs - left.requestedEndMs);
	const exact = candidates.filter(
		(candidate) =>
			candidate.requestedStartMs === draft.startMs &&
			candidate.requestedEndMs === draft.endMs &&
			candidate.burnInTimestamp === draft.burnInTimestamp &&
			(candidate.missingRanges.length > 0 && candidate.status !== 'partial') === draft.allowPartial
	);
	const exactActive = exact.find((candidate) => candidate.status === 'running') ?? null;
	const exactReady = exact.find((candidate) => candidate.status === 'ready') ?? null;
	const exactIds = new Set([exactActive?.id, exactReady?.id]);
	return {
		exactActive,
		exactReady,
		related: candidates.filter((candidate) => !exactIds.has(candidate.id))
	};
}

export function updateExportRange(
	range: ExportRange,
	startMs: number,
	endMs: number,
	bitrateKbps: number | null
): ExportRange {
	let orderedStartMs = Math.min(startMs, endMs);
	let orderedEndMs = Math.max(startMs, endMs);
	if (orderedEndMs - orderedStartMs > MAX_EXPORT_DURATION_MS) {
		const startChanged = startMs !== range.startMs;
		const endChanged = endMs !== range.endMs;
		if (startChanged !== endChanged) {
			const changedEndpoint = startChanged ? startMs : endMs;
			if (changedEndpoint === orderedStartMs) {
				orderedEndMs = orderedStartMs + MAX_EXPORT_DURATION_MS;
			} else {
				orderedStartMs = orderedEndMs - MAX_EXPORT_DURATION_MS;
			}
		} else {
			orderedEndMs = orderedStartMs + MAX_EXPORT_DURATION_MS;
		}
	}
	const durationMs = orderedEndMs - orderedStartMs;
	return {
		startMs: orderedStartMs,
		endMs: orderedEndMs,
		durationMs,
		estimatedBytes:
			bitrateKbps === null || bitrateKbps <= 0
				? null
				: Math.round((bitrateKbps * 1_000 * (durationMs / 1_000)) / 8)
	};
}

export function createSwimlaneWindow(
	inputs: readonly SwimlaneInput[],
	anchorMs: number,
	maximumLanes = 8
): SwimlaneWindow {
	if (!Number.isFinite(anchorMs)) throw new Error('Swimlane anchor must be finite');
	if (maximumLanes <= 0) throw new Error('Swimlane limit must be positive');

	const endMs = Math.floor(anchorMs / (15 * 60_000)) * (15 * 60_000) + 15 * 60_000;
	const startMs = endMs - SWIMLANE_WINDOW_MS;
	return {
		startMs,
		endMs,
		lanes: inputs.slice(0, maximumLanes).map((input) => ({
			cameraId: input.cameraId,
			availability: buildTimelineAvailability(input.segments, startMs, endMs)
		}))
	};
}
