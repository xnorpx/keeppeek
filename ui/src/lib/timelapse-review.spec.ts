import { describe, expect, it } from 'vitest';
import {
	MAX_TIMELAPSE_RANGE_MS,
	TimelapseThumbnailCache,
	TimelapseThumbnailQueue,
	resolveTimelapsePresetRange,
	timelapseGapLabel,
	type TimelapsePlanSample
} from './timelapse-review';

function sample(index: number): TimelapsePlanSample {
	return {
		kind: 'sample',
		index,
		requestedTimeMs: 1_772_962_200_000 + index * 60_000,
		actualTimeMs: 1_772_962_200_000 + index * 60_000,
		recordingId: 'rec-1',
		fragmentSequence: 1n,
		sampleOrdinal: index + 1,
		codec: 'avc1.64001f',
		width: 640,
		height: 360,
		encodedBytes: 4096,
		sampleToken: new Uint8Array([index & 0xff, 1, 2, 3]),
		tokenKey: `tok-${index}`
	};
}

describe('timelapse-review', () => {
	it('resolves presets in absolute UTC milliseconds across DST boundaries', () => {
		const springDstMs = Date.parse('2026-03-08T10:30:00Z');
		const lastHour = resolveTimelapsePresetRange('last_hour', springDstMs);
		expect(lastHour.endMs - lastHour.startMs).toBe(3_600_000);

		const fallDstMs = Date.parse('2026-11-01T09:30:00Z');
		const last15 = resolveTimelapsePresetRange('last_15m', fallDstMs);
		expect(last15.endMs - last15.startMs).toBe(900_000);

		expect(() =>
			resolveTimelapsePresetRange('custom', fallDstMs, {
				startMs: fallDstMs,
				endMs: fallDstMs - 1000
			})
		).toThrow();
		expect(() =>
			resolveTimelapsePresetRange('custom', fallDstMs, {
				startMs: fallDstMs - MAX_TIMELAPSE_RANGE_MS - 1,
				endMs: fallDstMs
			})
		).toThrow();
	});

	it('evicts oldest thumbnails past the cache byte cap and revokes object URLs', () => {
		const revoked: string[] = [];
		const cache = new TimelapseThumbnailCache(100, (url) => revoked.push(url));
		cache.set('a', 'blob:a', 60);
		cache.set('b', 'blob:b', 60);
		expect(cache.get('a')).toBeNull();
		expect(cache.get('b')).toBe('blob:b');
		expect(revoked).toEqual(['blob:a']);
		cache.clear();
		expect(revoked).toEqual(['blob:a', 'blob:b']);
		expect(cache.totalBytes).toBe(0);
	});

	it('bounds concurrent thumbnail loads and aborts stale fetches on camera or range reset', async () => {
		const revoked: string[] = [];
		const cache = new TimelapseThumbnailCache(1024 * 1024, (url) => revoked.push(url));
		let active = 0;
		let maxActive = 0;
		const resolvers: Array<() => void> = [];
		const abortedKeys: string[] = [];
		const updates: Array<{ key: string; url?: string; error?: string }> = [];

		const queue = new TimelapseThumbnailQueue(
			cache,
			(item, signal) =>
				new Promise((resolve, reject) => {
					active += 1;
					maxActive = Math.max(maxActive, active);
					signal.addEventListener('abort', () => {
						abortedKeys.push(item.tokenKey);
						active -= 1;
						reject(new Error('aborted'));
					});
					resolvers.push(() => {
						active -= 1;
						resolve({ url: `blob:${item.tokenKey}`, bytes: 100 });
					});
				}),
			(key, state) => updates.push({ key, ...state }),
			2
		);

		for (let i = 0; i < 6; i += 1) {
			queue.request(sample(i));
		}
		expect(maxActive).toBe(2);
		expect(queue.activeCount).toBe(2);
		expect(queue.pendingCount).toBe(4);

		// Complete first two and verify next two start cleanly without exceeding 2 concurrent.
		resolvers[0]!();
		resolvers[1]!();
		await Promise.resolve();
		await Promise.resolve();
		expect(maxActive).toBe(2);
		expect(updates.length).toBe(2);

		// Reset on camera/range change aborts active fetches, drops pending, and revokes cache.
		queue.reset();
		expect(abortedKeys).toEqual(['tok-2', 'tok-3']);
		expect(queue.activeCount).toBe(0);
		expect(queue.pendingCount).toBe(0);
		expect(revoked).toEqual(['blob:tok-0', 'blob:tok-1']);
	});

	it('provides explicit labels for every gap classification', () => {
		expect(timelapseGapLabel('missing')).toContain('No recorded');
		expect(timelapseGapLabel('retained')).toContain('retention');
		expect(timelapseGapLabel('privacy')).toContain('Privacy');
		expect(timelapseGapLabel('unsupported_codec')).toContain('codec');
		expect(timelapseGapLabel('corrupt')).toContain('Unreadable');
		expect(timelapseGapLabel('open_recording')).toContain('open');
	});
});
