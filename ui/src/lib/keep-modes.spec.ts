import { describe, expect, it } from 'vitest';
import type { RecordingEvent, RecordingSegment } from './types';
import {
	classifyExportCandidates,
	createEventExportRange,
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
		expect(createExportRange(segment('front-door', 0, 300_000), 8_000)).toEqual({
			startMs: 0,
			endMs: 120_000,
			durationMs: 120_000,
			estimatedBytes: 120_000_000
		});
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
		expect(updateExportRange(original, 0, 300_000, null)).toEqual({
			startMs: 180_000,
			endMs: 300_000,
			durationMs: 120_000,
			estimatedBytes: null
		});
		expect(updateExportRange(original, -180_000, 120_000, null)).toEqual({
			startMs: -180_000,
			endMs: -60_000,
			durationMs: 120_000,
			estimatedBytes: null
		});
	});

	it('seeds a bounded fifteen-second context around an event', () => {
		expect(createEventExportRange({ start_time_ms: 60_000, end_time_ms: 75_000 }, 8_000)).toEqual({
			startMs: 45_000,
			endMs: 90_000,
			durationMs: 45_000,
			estimatedBytes: 45_000_000
		});
		expect(createEventExportRange({ start_time_ms: 60_000, end_time_ms: 300_000 }, null)).toEqual({
			startMs: 45_000,
			endMs: 165_000,
			durationMs: 120_000,
			estimatedBytes: null
		});
	});

	it('classifies only reusable exact jobs and keeps overlaps advisory', () => {
		const base = {
			sourceId: 'front-door',
			streamId: 'main' as const,
			requestedStartMs: 1_000,
			requestedEndMs: 2_000,
			expiresAtMs: 10_000,
			fileName: 'export.mp4',
			sha256: 'checksum',
			missingRanges: [],
			burnInTimestamp: false
		};
		const result = classifyExportCandidates(
			[
				{ ...base, id: 'active', status: 'running' as const },
				{ ...base, id: 'ready', status: 'ready' as const },
				{
					...base,
					id: 'different-options',
					status: 'ready' as const,
					burnInTimestamp: true
				},
				{
					...base,
					id: 'overlap',
					status: 'ready' as const,
					requestedStartMs: 1_500,
					requestedEndMs: 2_500
				},
				{ ...base, id: 'expired', status: 'expired' as const },
				{
					...base,
					id: 'elapsed',
					status: 'ready' as const,
					expiresAtMs: 500
				}
			],
			{
				sourceId: 'front-door',
				streamId: 'main',
				startMs: 1_000,
				endMs: 2_000,
				allowPartial: false,
				burnInTimestamp: false
			},
			1_000
		);

		expect(result.exactActive?.id).toBe('active');
		expect(result.exactReady?.id).toBe('ready');
		expect(result.related.map((candidate) => candidate.id)).toEqual([
			'overlap',
			'different-options'
		]);
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
