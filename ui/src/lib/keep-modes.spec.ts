import { describe, expect, it } from 'vitest';
import type { RecordingEvent, RecordingSegment } from './types';
import {
	createExportRange,
	createSwimlaneWindow,
	parseKeepMode,
	storyEvents,
	updateExportRange
} from './keep-modes';

function segment(cameraId: string, startMs: number, endMs: number): RecordingSegment {
	return {
		stream: 'main',
		date: '2026-08-18',
		hour: '06',
		filename: `${cameraId}.mp4`,
		url: `/${cameraId}.mp4`,
		start_time_ms: startMs,
		end_time_ms: endMs,
		duration_ms: endMs - startMs
	};
}

function event(id: string, kind: string, startTimeMs: number): RecordingEvent {
	return {
		id,
		source: 'camera',
		kind,
		start_time_ms: startTimeMs,
		end_time_ms: null,
		confidence: null,
		bbox: null,
		zone: null,
		thumbnail_url: null
	};
}

describe('Keep modes', () => {
	it('falls back to timeline for unknown URL modes', () => {
		expect(parseKeepMode('stories')).toBe('stories');
		expect(parseKeepMode('unknown')).toBe('timeline');
		expect(parseKeepMode(null)).toBe('timeline');
	});

	it('selects only exact server-authored story events, newest first', () => {
		expect(
			storyEvents([
				event('motion', 'motion', 30),
				event('old', 'story', 10),
				event('new', 'Story', 20)
			]).map((item) => item.id)
		).toEqual(['new', 'old']);
	});

	it('estimates export bytes only when the camera reports bitrate', () => {
		const source = segment('front-door', 0, 120_000);

		expect(createExportRange(source, 8_000)).toEqual({
			startMs: 0,
			endMs: 120_000,
			durationMs: 120_000,
			estimatedBytes: 120_000_000
		});
		expect(createExportRange(source, null)?.estimatedBytes).toBeNull();
	});

	it('orders edited export handles without inventing bitrate', () => {
		const original = createExportRange(segment('front-door', 0, 120_000), null);
		if (original === null) throw new Error('Expected an export range');

		expect(updateExportRange(original, 90_000, 30_000, null)).toEqual({
			startMs: 30_000,
			endMs: 90_000,
			durationMs: 60_000,
			estimatedBytes: null
		});
	});

	it('builds at most eight lanes on one quarter-hour aligned clock', () => {
		const hour = 60 * 60_000;
		const inputs = Array.from({ length: 10 }, (_, index) => ({
			cameraId: `camera-${index}`,
			segments: [segment(`camera-${index}`, hour + index * 1_000, hour + 30_000)]
		}));

		const window = createSwimlaneWindow(inputs, hour + 37 * 60_000);

		expect(window.startMs).toBe(45 * 60_000);
		expect(window.endMs).toBe(hour + 45 * 60_000);
		expect(window.lanes).toHaveLength(8);
		expect(window.lanes[0].availability.available).toHaveLength(1);
	});
});
