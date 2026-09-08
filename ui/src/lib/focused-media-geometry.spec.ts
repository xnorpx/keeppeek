import { describe, expect, it } from 'vitest';
import {
	clampMediaTransform,
	fitMediaSize,
	pinchMediaTransform,
	resetMediaTransform,
	resizeMediaTransform,
	zoomMediaAt
} from './focused-media-geometry';

describe('fitMediaSize', () => {
	it('fits wide media without cropping or stretching', () => {
		expect(fitMediaSize({ width: 800, height: 600 }, { width: 1920, height: 1080 })).toEqual({
			width: 800,
			height: 450
		});
	});

	it('fits portrait media without allowing horizontal letterboxing into the pan bounds', () => {
		expect(fitMediaSize({ width: 800, height: 600 }, { width: 1080, height: 1920 })).toEqual({
			width: 337.5,
			height: 600
		});
	});

	it('recomputes the full frame after a viewport orientation change', () => {
		const media = { width: 1920, height: 1080 };
		expect(fitMediaSize({ width: 800, height: 450 }, media)).toEqual({
			width: 800,
			height: 450
		});
		expect(fitMediaSize({ width: 450, height: 800 }, media)).toEqual({
			width: 450,
			height: 253.125
		});
	});

	it.each([0, -1, Number.NaN, Number.POSITIVE_INFINITY])(
		'keeps the viewport before valid intrinsic dimensions arrive: %s',
		(invalid) => {
			const viewport = { width: 640, height: 360 };
			expect(fitMediaSize(viewport, { width: invalid, height: 1080 })).toEqual(viewport);
			expect(fitMediaSize(viewport, { width: 1920, height: invalid })).toEqual(viewport);
		}
	);

	it.each([0, -1, Number.NaN, Number.POSITIVE_INFINITY])(
		'never emits invalid geometry for a hidden or invalid viewport: %s',
		(invalid) => {
			const media = { width: 1920, height: 1080 };
			expect(fitMediaSize({ width: invalid, height: 360 }, media)).toEqual({ width: 0, height: 0 });
			expect(fitMediaSize({ width: 640, height: invalid }, media)).toEqual({ width: 0, height: 0 });
		}
	);
});

describe('media transforms', () => {
	const bounds = { width: 640, height: 360 };

	it('clamps both translation axes to the visible media at both scale limits', () => {
		expect(clampMediaTransform({ scale: 100, panX: 10_000, panY: -10_000 }, bounds)).toEqual({
			scale: 8,
			panX: 2240,
			panY: -1260
		});
		expect(clampMediaTransform({ scale: -1, panX: 100, panY: 100 }, bounds)).toEqual(
			resetMediaTransform()
		);
	});

	it('preserves the image coordinate under the zoom focal point', () => {
		const initial = { scale: 2, panX: 40, panY: -20 };
		const point = { x: 160, y: 90 };
		const zoomed = zoomMediaAt(initial, 4, point, bounds);
		expect((point.x - zoomed.panX) / zoomed.scale).toBe((point.x - initial.panX) / initial.scale);
		expect((point.y - zoomed.panY) / zoomed.scale).toBe((point.y - initial.panY) / initial.scale);
	});

	it('preserves the moving pinch centroid while zooming and panning together', () => {
		const result = pinchMediaTransform(
			resetMediaTransform(),
			[
				{ x: -50, y: 0 },
				{ x: 50, y: 0 }
			],
			[
				{ x: -50, y: 30 },
				{ x: 150, y: 30 }
			],
			bounds
		);
		expect(result).toEqual({ scale: 2, panX: 50, panY: 30 });
	});

	it('ignores a degenerate pinch instead of producing non-finite transforms', () => {
		const initial = { scale: 2, panX: 40, panY: 20 };
		expect(
			pinchMediaTransform(
				initial,
				[
					{ x: 0, y: 0 },
					{ x: 0, y: 0 }
				],
				[
					{ x: -50, y: 0 },
					{ x: 50, y: 0 }
				],
				bounds
			)
		).toEqual(initial);
	});

	it('preserves normalized inspection position through resize and orientation changes', () => {
		expect(
			resizeMediaTransform({ scale: 2, panX: 100, panY: -50 }, bounds, {
				width: 320,
				height: 180
			})
		).toEqual({ scale: 2, panX: 50, panY: -25 });
		expect(
			resizeMediaTransform(resetMediaTransform(), bounds, { width: 180, height: 320 })
		).toEqual(resetMediaTransform());
	});

	it.each([Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY])(
		'rejects non-finite transform input: %s',
		(invalid) => {
			expect(clampMediaTransform({ scale: invalid, panX: 0, panY: 0 }, bounds)).toEqual(
				resetMediaTransform()
			);
			expect(clampMediaTransform({ scale: 2, panX: invalid, panY: invalid }, bounds)).toEqual({
				scale: 2,
				panX: 0,
				panY: 0
			});
		}
	);
});
