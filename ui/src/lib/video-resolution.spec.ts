import { describe, expect, it } from 'vitest';
import { videoResolutionMatches } from './video-resolution';

describe('videoResolutionMatches', () => {
	it('accepts exact dimensions and small codec edge cropping', () => {
		expect(videoResolutionMatches('640x360', 640, 360)).toBe(true);
		expect(videoResolutionMatches('640x360', 640, 352)).toBe(true);
		expect(videoResolutionMatches('4096×1860', 4096, 1856)).toBe(true);
	});

	it('rejects a retained main frame for a requested substream', () => {
		expect(videoResolutionMatches('1200x536', 4096, 1860)).toBe(false);
		expect(videoResolutionMatches('640x360', 1200, 536)).toBe(false);
	});

	it('does not block readiness when profile dimensions are unavailable', () => {
		expect(videoResolutionMatches(null, 640, 360)).toBe(true);
		expect(videoResolutionMatches('unknown', 640, 360)).toBe(true);
	});
});
