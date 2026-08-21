import type { RecordingSegment } from './types';

export type TimelineAvailabilityRange = {
	startMs: number;
	endMs: number;
	segmentUrls: readonly string[];
};

export type TimelineGapRange = {
	startMs: number;
	endMs: number;
};

export type TimelineAvailability = {
	available: readonly TimelineAvailabilityRange[];
	gaps: readonly TimelineGapRange[];
};

export function buildTimelineAvailability(
	segments: readonly RecordingSegment[],
	windowStartMs: number,
	windowEndMs: number
): TimelineAvailability {
	if (windowEndMs <= windowStartMs) throw new Error('Timeline window must have positive duration');

	const clipped = segments
		.flatMap((segment) => {
			const startMs = Math.max(windowStartMs, segment.start_time_ms);
			const endMs = Math.min(windowEndMs, segment.end_time_ms);
			return endMs > startMs ? [{ startMs, endMs, segmentUrls: [segment.url] }] : [];
		})
		.toSorted((left, right) => left.startMs - right.startMs);

	const available: TimelineAvailabilityRange[] = [];
	for (const range of clipped) {
		const previous = available.at(-1);
		if (previous !== undefined && range.startMs <= previous.endMs) {
			previous.endMs = Math.max(previous.endMs, range.endMs);
			previous.segmentUrls = [...previous.segmentUrls, ...range.segmentUrls];
		} else {
			available.push({ ...range });
		}
	}

	const gaps: TimelineGapRange[] = [];
	let cursorMs = windowStartMs;
	for (const range of available) {
		if (range.startMs > cursorMs) gaps.push({ startMs: cursorMs, endMs: range.startMs });
		cursorMs = range.endMs;
	}
	if (cursorMs < windowEndMs) gaps.push({ startMs: cursorMs, endMs: windowEndMs });

	return { available, gaps };
}
