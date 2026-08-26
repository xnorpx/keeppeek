import { describe, expect, it } from 'vitest';
import {
	browserSupportsLiveEncoding,
	browserSupportsRecordedEncoding,
	preferredRecordedStream,
	selectRecordedStream
} from './recorded-playback-policy';
import type { ProfileSummary } from './types';

function profile(
	stream: 'main' | 'sub',
	encoding: string | null,
	resolution = stream === 'main' ? '3840x2160' : '640x360'
): ProfileSummary {
	return {
		name: stream,
		stream,
		encoding,
		resolution,
		framerate: stream === 'main' ? 25 : 15
	};
}

describe('preferredRecordedStream', () => {
	it('prefers a compatible main recording over a compatible substream', () => {
		expect(
			preferredRecordedStream({ profiles: [profile('main', 'h264'), profile('sub', 'h264')] })
		).toBe('main');
	});

	it('uses a compatible substream when main is not browser compatible', () => {
		expect(
			preferredRecordedStream({ profiles: [profile('main', 'h265'), profile('sub', 'h264')] })
		).toBe('sub');
	});

	it('falls back to the available main profile when compatibility is unknown', () => {
		expect(preferredRecordedStream({ profiles: [profile('main', null)] })).toBe('main');
	});

	it('defaults to main when no profile metadata is available', () => {
		expect(preferredRecordedStream({ profiles: [] })).toBe('main');
	});

	it('honors an explicit compatible stream before a remembered preference', () => {
		const selection = selectRecordedStream(
			{ profiles: [profile('main', 'h264'), profile('sub', 'h264')] },
			{
				availableStreams: ['main', 'sub'],
				requestedStream: 'sub',
				preference: 'high'
			}
		);

		expect(selection.selectedStream).toBe('sub');
		expect(selection.reason).toBe('explicit');
		expect(selection.fallbackStreams).toEqual(['main']);
	});

	it('ignores an explicit unsupported stream and selects a compatible fallback', () => {
		const selection = selectRecordedStream(
			{ profiles: [profile('main', 'h265'), profile('sub', 'h264')] },
			{ availableStreams: ['main', 'sub'], requestedStream: 'main' }
		);

		expect(selection.selectedStream).toBe('sub');
		expect(selection.reason).toBe('automatic');
		expect(selection.rejectedStreams).toEqual([{ stream: 'main', encoding: 'h265' }]);
	});

	it('honors a remembered low preference without treating it as an exact stream', () => {
		const selection = selectRecordedStream(
			{
				profiles: [profile('main', 'h264', '1920x1080'), profile('sub', 'h264', '640x360')]
			},
			{ availableStreams: ['main', 'sub'], preference: 'low' }
		);

		expect(selection.selectedStream).toBe('sub');
		expect(selection.reason).toBe('preference');
	});

	it('ignores an unavailable exact preference and chooses the best recorded variant', () => {
		const selection = selectRecordedStream(
			{ profiles: [profile('main', 'h264'), profile('sub', 'h264')] },
			{ availableStreams: ['main'], preference: 'sub' }
		);

		expect(selection.selectedStream).toBe('main');
		expect(selection.reason).toBe('automatic');
	});

	it('prefers main when compatible variants have equal quality rank', () => {
		const selection = selectRecordedStream(
			{
				profiles: [profile('sub', 'h264', '1920x1080'), profile('main', 'h264', '1920x1080')]
			},
			{ availableStreams: ['main', 'sub'] }
		);

		expect(selection.selectedStream).toBe('main');
		expect(selection.fallbackStreams).toEqual(['sub']);
	});

	it('uses the advertised quality rank before inferred resolution', () => {
		const main = profile('main', 'h264', '1280x720');
		const sub = profile('sub', 'h264', '1920x1080');
		main.quality_rank = 3;
		sub.quality_rank = 1;
		const selection = selectRecordedStream(
			{ profiles: [sub, main] },
			{ availableStreams: ['main', 'sub'] }
		);

		expect(selection.selectedStream).toBe('main');
		expect(selection.fallbackStreams).toEqual(['sub']);
	});

	it('uses available recordings when profile metadata is missing', () => {
		const selection = selectRecordedStream({ profiles: [] }, { availableStreams: ['sub'] });

		expect(selection.selectedStream).toBe('sub');
		expect(selection.reason).toBe('automatic');
	});

	it('uses MediaSource support for exact AVC and HEVC codec identifiers', () => {
		const originalMediaSource = globalThis.MediaSource;
		class FakeMediaSource {
			static isTypeSupported(contentType: string): boolean {
				return contentType.includes('avc1.640028') || contentType.includes('hvc1.1.6.L120.B0');
			}
		}
		Object.assign(globalThis, { MediaSource: FakeMediaSource });
		try {
			expect(browserSupportsRecordedEncoding('avc1.640028')).toBe(true);
			expect(browserSupportsRecordedEncoding('H.265')).toBe(true);
			expect(browserSupportsRecordedEncoding('vp9')).toBe(false);
		} finally {
			Object.assign(globalThis, { MediaSource: originalMediaSource });
		}
	});

	it('uses WebRTC receiver capabilities for focused-live codecs', () => {
		const originalReceiver = globalThis.RTCRtpReceiver;
		class FakeReceiver {
			static getCapabilities(): RTCRtpCapabilities {
				return {
					codecs: [
						{
							channels: 0,
							clockRate: 90_000,
							mimeType: 'video/H264',
							sdpFmtpLine: ''
						}
					],
					headerExtensions: []
				};
			}
		}
		Object.assign(globalThis, { RTCRtpReceiver: FakeReceiver });
		try {
			expect(browserSupportsLiveEncoding('h264')).toBe(true);
			expect(browserSupportsLiveEncoding('h265')).toBe(false);
		} finally {
			Object.assign(globalThis, { RTCRtpReceiver: originalReceiver });
		}
	});
});
