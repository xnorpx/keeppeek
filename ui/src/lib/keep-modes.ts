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
	const durationMs = segment.end_time_ms - segment.start_time_ms;
	return {
		startMs: segment.start_time_ms,
		endMs: segment.end_time_ms,
		durationMs,
		estimatedBytes:
			bitrateKbps === null || bitrateKbps <= 0
				? null
				: Math.round((bitrateKbps * 1_000 * (durationMs / 1_000)) / 8)
	};
}

export function updateExportRange(
	range: ExportRange,
	startMs: number,
	endMs: number,
	bitrateKbps: number | null
): ExportRange {
	const orderedStartMs = Math.min(startMs, endMs);
	const orderedEndMs = Math.max(startMs, endMs);
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
