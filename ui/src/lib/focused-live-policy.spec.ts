import { describe, expect, it } from 'vitest';
import { canUseFocusedNetworkAuto, matchesFocusedLiveFrame } from './focused-live-policy';
import type { FocusedLivePreference } from './playback-preferences';
import type { ProfileSummary } from './types';

const profiles: ProfileSummary[] = ['main', 'sub'].map((stream) => ({
	name: stream,
	stream: stream as 'main' | 'sub',
	encoding: 'h264',
	resolution: '640x360',
	framerate: 15
}));
const automatic = {
	preference: 'auto' as const,
	previewPresented: true,
	fallbackStream: null,
	isEncodingSupported: (encoding: string) => encoding === 'h264'
};

describe('focused native network Auto eligibility', () => {
	it.each([{ profiles }, { profiles: [profiles[0]] }, { profiles: [profiles[1]] }])(
		'permits every fully compatible advertised native topology',
		({ profiles: native }) => {
			expect(canUseFocusedNetworkAuto({ profiles: native }, automatic)).toBe(true);
		}
	);
	it.each(['high', 'low', 'main', 'sub'] as FocusedLivePreference[])(
		'preserves explicit %s intent',
		(preference) => {
			expect(canUseFocusedNetworkAuto({ profiles }, { ...automatic, preference })).toBe(false);
		}
	);
	it('keeps preview, runtime fallback and partially compatible cameras exact', () => {
		expect(canUseFocusedNetworkAuto({ profiles }, { ...automatic, previewPresented: false })).toBe(
			false
		);
		expect(canUseFocusedNetworkAuto({ profiles }, { ...automatic, fallbackStream: 'sub' })).toBe(
			false
		);
		expect(
			canUseFocusedNetworkAuto(
				{ profiles: [{ ...profiles[0], encoding: 'h265' }, profiles[1]] },
				automatic
			)
		).toBe(false);
	});
	it('requires known, unique native profiles before delegating selection', () => {
		expect(canUseFocusedNetworkAuto(null, automatic)).toBe(false);
		expect(canUseFocusedNetworkAuto({ profiles: [] }, automatic)).toBe(false);
		expect(
			canUseFocusedNetworkAuto({ profiles: [{ ...profiles[0], encoding: null }] }, automatic)
		).toBe(false);
		expect(canUseFocusedNetworkAuto({ profiles: [profiles[0], profiles[0]] }, automatic)).toBe(
			false
		);
		expect(
			canUseFocusedNetworkAuto(
				{ profiles: [{ ...profiles[0], stream: 'published' as 'main' }] },
				automatic
			)
		).toBe(false);
	});
});

describe('focused frame handoff', () => {
	const track = { requestedVariantId: null, activeStream: 'sub' as const, pendingStream: null };
	const frame = { width: 640, height: 360, stream: 'sub' as const, status: 'live' as const };
	const expected = { automatic: true, stream: 'sub' as const, resolution: '640x360' };
	it('accepts the actually admitted Auto stream without an exact variant request', () => {
		expect(matchesFocusedLiveFrame(track, frame, expected)).toBe(true);
		expect(track).toEqual({ requestedVariantId: null, activeStream: 'sub', pendingStream: null });
	});
	it('rejects mismatched identity, pending switches, wrong dimensions and non-live frames', () => {
		expect(matchesFocusedLiveFrame({ ...track, requestedVariantId: 'sub' }, frame, expected)).toBe(
			false
		);
		expect(matchesFocusedLiveFrame({ ...track, activeStream: 'main' }, frame, expected)).toBe(
			false
		);
		expect(matchesFocusedLiveFrame({ ...track, pendingStream: 'main' }, frame, expected)).toBe(
			false
		);
		expect(matchesFocusedLiveFrame(track, { ...frame, stream: 'main' }, expected)).toBe(false);
		expect(matchesFocusedLiveFrame(track, { ...frame, width: 1920, height: 1080 }, expected)).toBe(
			false
		);
		expect(matchesFocusedLiveFrame(track, { ...frame, status: 'connecting' }, expected)).toBe(
			false
		);
	});
	it('retains the exact variant requirement for manual and preview frames', () => {
		expect(matchesFocusedLiveFrame(track, frame, { ...expected, automatic: false })).toBe(false);
		expect(
			matchesFocusedLiveFrame({ ...track, requestedVariantId: 'sub' }, frame, {
				...expected,
				automatic: false
			})
		).toBe(true);
	});
});
