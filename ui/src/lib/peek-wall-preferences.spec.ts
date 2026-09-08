import { describe, expect, it } from 'vitest';
import {
	defaultPeekWallPreferences,
	peekWallAppearancePreset,
	peekWallAppearancePresets,
	stableNativeRatio,
	wallDisplayFromWire,
	wallDisplayToWire
} from './peek-wall-preferences';

describe('Peek wall preferences', () => {
	it('round-trips every server-owned display field without losing custom spacing', () => {
		const preferences = {
			...defaultPeekWallPreferences(),
			tileShape: 'native' as const,
			mediaFit: 'cover' as const,
			streamingMode: 'continuous' as const,
			keepAwake: true,
			streamLimit: 6,
			gapPx: 7,
			cornerRadiusPx: 18
		};
		const wire = wallDisplayToWire(preferences);
		expect(wire).toEqual({
			version: 1,
			tile_shape: 'native',
			media_fit: 'cover',
			streaming_mode: 'continuous',
			keep_awake: true,
			stream_limit: 6,
			gap_px: 7,
			corner_radius_px: 18
		});
		expect(wallDisplayFromWire(wire)).toEqual(preferences);
	});

	it('derives preset selection from the actual gap and radius', () => {
		for (const preset of peekWallAppearancePresets) {
			expect(peekWallAppearancePreset(preset)).toBe(preset.id);
		}
		expect(peekWallAppearancePreset({ gapPx: 7, cornerRadiusPx: 18 })).toBe('custom');
	});

	it.each([
		['version', 2],
		['tile_shape', 'stretch'],
		['media_fit', { cover: null }],
		['streaming_mode', 'unlimited'],
		['keep_awake', 'true'],
		['stream_limit', 0],
		['stream_limit', 13],
		['gap_px', -1],
		['gap_px', 25],
		['gap_px', 1.5],
		['corner_radius_px', '2'],
		['corner_radius_px', 25],
		['extra', true]
	])('rejects invalid server field %s', (field, value) => {
		const wire = { ...wallDisplayToWire(defaultPeekWallPreferences()), [field]: value };
		expect(() => wallDisplayFromWire(wire)).toThrow();
	});

	it.each([0, 24])('accepts the %i px appearance boundary', (value) => {
		const preferences = { ...defaultPeekWallPreferences(), gapPx: value, cornerRadiusPx: value };
		expect(wallDisplayFromWire(wallDisplayToWire(preferences))).toEqual(preferences);
	});

	it('defaults to uncropped 16:9, Smart streaming, automatic capacity, and no wake lock', () => {
		expect(defaultPeekWallPreferences()).toEqual({
			version: 1,
			tileShape: '16:9',
			mediaFit: 'contain',
			streamingMode: 'smart',
			streamLimit: 12,
			keepAwake: false,
			gapPx: 10,
			cornerRadiusPx: 10
		});
	});

	it.each([null, [], {}, { version: 2 }, 'x'.repeat(4_097)].map((value) => ({ value })))(
		'rejects malformed or unsupported server documents',
		({ value }) => {
			expect(() => wallDisplayFromWire(value)).toThrow();
		}
	);

	it('latches valid native dimensions so quality changes cannot repack the wall', () => {
		expect(stableNativeRatio(null, 0, 0)).toBeNull();
		expect(stableNativeRatio(null, Infinity, 720)).toBeNull();
		expect(stableNativeRatio(null, 1, 100_000)).toBeNull();
		expect(stableNativeRatio(null, 1_080, 1_920)).toBe(9 / 16);
		expect(stableNativeRatio(null, 3_840, 480)).toBe(8);
		expect(stableNativeRatio(4 / 3, 1_920, 1_080)).toBe(4 / 3);
	});
});
