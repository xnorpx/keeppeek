import { describe, expect, it } from 'vitest';
import {
	expectedGridEpochMs,
	GridPlaybackSession,
	needsGridDriftCorrection
} from './grid-playback-session';

describe('GridPlaybackSession', () => {
	it('queues every replay during scrub and admits visible cameras by budget after commit', () => {
		const session = new GridPlaybackSession();
		const demands = [
			{ cameraId: 'front', visibleFraction: 1, focused: false, hasRecording: true },
			{ cameraId: 'garage', visibleFraction: 0.5, focused: false, hasRecording: true },
			{ cameraId: 'yard', visibleFraction: 0.4, focused: false, hasRecording: true }
		];
		session.update({ mode: 'scrub', selectedEpochMs: 10_000, playbackRate: 1 });
		expect(session.reconcile(demands, 2)).toEqual({
			activeCameraIds: [],
			queuedCameraIds: ['front', 'garage', 'yard']
		});

		session.update({ mode: 'playback', selectedEpochMs: 10_000, playbackRate: 1 });
		expect(session.reconcile(demands, 2)).toEqual({
			activeCameraIds: ['front', 'garage'],
			queuedCameraIds: ['yard']
		});
	});

	it('lets a focused replay preempt the lowest visible grant', () => {
		const session = new GridPlaybackSession();
		session.update({ mode: 'paused', selectedEpochMs: 10_000, playbackRate: 1 });
		expect(
			session.reconcile(
				[
					{ cameraId: 'front', visibleFraction: 1, focused: false, hasRecording: true },
					{ cameraId: 'yard', visibleFraction: 0, focused: true, hasRecording: true }
				],
				1
			)
		).toEqual({ activeCameraIds: ['yard'], queuedCameraIds: ['front'] });
	});

	it('preserves a shared absolute playhead across pause, resume, seek, and rate changes', () => {
		const session = new GridPlaybackSession();
		session.update({
			mode: 'playback',
			selectedEpochMs: 100_000,
			playbackRate: 1,
			monotonicMs: 500
		});
		expect(session.positionAt(1_000)).toBe(100_500);
		session.setRate(2, 1_000);
		expect(session.positionAt(1_250)).toBe(101_000);
		session.setPlaying(false, 1_250);
		expect(session.positionAt(20_000)).toBe(101_000);
		session.seek(42_000, 20_000);
		session.setPlaying(true, 20_100);
		expect(session.positionAt(20_200)).toBe(42_200);
		expect(() => session.setRate(Infinity, 20_200)).toThrow(RangeError);
		expect(() => session.seek(-1, 20_200)).toThrow(RangeError);
		expect(session.positionAt(20_200)).toBe(42_200);
	});

	it('limits replay to four distinct cameras and rejects a non-finite decoder budget', () => {
		const session = new GridPlaybackSession();
		const demands = ['a', 'b', 'b', 'c', 'd', 'e', 'f'].map((cameraId) => ({
			cameraId,
			visibleFraction: 1,
			focused: cameraId === 'f',
			hasRecording: true
		}));
		session.update({ mode: 'playback', selectedEpochMs: 10_000, playbackRate: 1 });
		expect(session.reconcile(demands, 127)).toEqual({
			activeCameraIds: ['f', 'a', 'b', 'c'],
			queuedCameraIds: ['d', 'e']
		});
		expect(session.reconcile(demands, Infinity).activeCameraIds).toEqual([]);
	});

	it('uses a monotonic shared epoch and corrects only material drift', () => {
		expect(expectedGridEpochMs(10_000, 1_000, 1_500, 2)).toBe(11_000);
		expect(needsGridDriftCorrection(100)).toBe(false);
		expect(needsGridDriftCorrection(250)).toBe(false);
		expect(needsGridDriftCorrection(251)).toBe(true);
		expect(needsGridDriftCorrection(-300)).toBe(true);
	});
});
