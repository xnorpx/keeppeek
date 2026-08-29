import { describe, expect, it } from 'vitest';
import { rangePosition, summarizeCamera } from './recording-coverage';
import type { CameraRecordingCoverage } from './types';

describe('recording coverage presentation', () => {
	it('summarizes the weakest requested stream without counting policy-disabled gaps', () => {
		const camera = {
			state: 'degraded',
			streams: [
				{
					recording_requested: true,
					playable_fragments: 2,
					coverage_percent: 98.5,
					recording_bytes: 1_000,
					estimated_bytes_per_day: 500,
					effective_retention_ms: 86_400_000,
					gap_count: 2,
					largest_gap_ms: 10_000
				},
				{
					recording_requested: false,
					playable_fragments: 1,
					coverage_percent: 50,
					recording_bytes: 200,
					estimated_bytes_per_day: 100,
					effective_retention_ms: 43_200_000,
					gap_count: 9,
					largest_gap_ms: 20_000
				}
			]
		} as CameraRecordingCoverage;

		expect(summarizeCamera(camera)).toEqual({
			coveragePercent: 98.5,
			recordingBytes: 1_200,
			estimatedBytesPerDay: 600,
			effectiveRetentionMs: 86_400_000,
			gapCount: 2,
			largestGapMs: 10_000
		});
	});

	it('clips coverage geometry to the selected half-open window', () => {
		expect(rangePosition(500, 1_500, 1_000, 3_000)).toEqual({ left: 0, width: 25 });
		expect(rangePosition(2_500, 4_000, 1_000, 3_000)).toEqual({ left: 75, width: 25 });
	});

	it('reports missing retention evidence instead of an infinite duration', () => {
		const camera = {
			streams: [
				{
					recording_requested: true,
					playable_fragments: 1,
					coverage_percent: 100,
					recording_bytes: 10,
					estimated_bytes_per_day: 10,
					effective_retention_ms: null,
					gap_count: 0,
					largest_gap_ms: 0
				}
			]
		} as CameraRecordingCoverage;

		expect(summarizeCamera(camera).effectiveRetentionMs).toBeNull();
	});
});
