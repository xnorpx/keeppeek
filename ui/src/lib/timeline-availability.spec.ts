import { describe, expect, it } from 'vitest';
import type { RecordingSegment } from './types';
import { buildTimelineAvailability } from './timeline-availability';

function segment(url: string, startMs: number, endMs: number): RecordingSegment {
	return {
		stream: 'main',
		date: '2026-08-10',
		hour: '00',
		filename: `${url}.mp4`,
		url,
		start_time_ms: startMs,
		end_time_ms: endMs,
		duration_ms: endMs - startMs
	};
}

describe('buildTimelineAvailability', () => {
	it('merges touching and overlapping recording segments', () => {
		const result = buildTimelineAvailability(
			[segment('a', 10, 20), segment('b', 20, 30), segment('c', 25, 40)],
			0,
			50
		);

		expect(result.available).toEqual([{ startMs: 10, endMs: 40, segmentUrls: ['a', 'b', 'c'] }]);
		expect(result.gaps).toEqual([
			{ startMs: 0, endMs: 10 },
			{ startMs: 40, endMs: 50 }
		]);
	});

	it('clips recordings to the visible timeline window', () => {
		const result = buildTimelineAvailability(
			[segment('before', -10, 5), segment('after', 15, 30)],
			0,
			20
		);

		expect(result.available).toEqual([
			{ startMs: 0, endMs: 5, segmentUrls: ['before'] },
			{ startMs: 15, endMs: 20, segmentUrls: ['after'] }
		]);
		expect(result.gaps).toEqual([{ startMs: 5, endMs: 15 }]);
	});

	it('renders the entire window as an explicit gap when footage is absent', () => {
		expect(buildTimelineAvailability([], 0, 50)).toEqual({
			available: [],
			gaps: [{ startMs: 0, endMs: 50 }]
		});
	});

	it('rejects an invalid window', () => {
		expect(() => buildTimelineAvailability([], 10, 10)).toThrow(
			'Timeline window must have positive duration'
		);
	});
});
