import { describe, expect, it, vi } from 'vitest';
import { AacAudioPlayback } from './audio-playback';
import { create } from '@bufbuild/protobuf';
import { AudioDataFrameSchema } from './proto/webrtc_pb';

function installAacPlaybackClock() {
	const start = vi.fn();
	const stop = vi.fn();
	vi.stubGlobal(
		'AudioContext',
		class {
			currentTime = 1;
			createBuffer() {
				return { duration: 0.02, getChannelData: () => new Float32Array(1) };
			}
			createBufferSource() {
				return { connect: vi.fn(), start, stop };
			}
			close() {
				return Promise.resolve();
			}
		}
	);
	vi.stubGlobal(
		'AudioDecoder',
		class {
			static async isConfigSupported() {
				return { supported: true };
			}
			constructor(readonly init: AudioDecoderInit) {}
			configure() {}
			close() {}
			decode(chunk: EncodedAudioChunk) {
				this.init.output({
					timestamp: chunk.timestamp,
					numberOfChannels: 1,
					numberOfFrames: 960,
					sampleRate: 48_000,
					copyTo: vi.fn(),
					close: vi.fn()
				} as unknown as AudioData);
			}
		}
	);
	vi.stubGlobal(
		'EncodedAudioChunk',
		class {
			readonly timestamp: number;
			constructor(init: EncodedAudioChunkInit) {
				this.timestamp = init.timestamp;
			}
		}
	);
	return { start, stop };
}

describe('reliable AAC presentation', () => {
	it('applies changes to saved timing while earlier decoded audio is still buffered', async () => {
		vi.useFakeTimers();
		const { start, stop } = installAacPlaybackClock();
		const playback = new AacAudioPlayback();
		try {
			await playback.configure('aac', new Uint8Array([0x12, 0x10]), 48_000, 1);
			// The same binding goes from saved 0 to +80 ms, then back to 0.
			for (const [index, ms] of [100, 120, 220, 240, 180].entries()) {
				playback.push(
					create(AudioDataFrameSchema, {
						frameId: BigInt(index + 1),
						timestamp: { seconds: 10n, nanos: ms * 1_000_000 },
						duration: { nanos: 20_000_000 },
						payload: new Uint8Array([1])
					})
				);
				await vi.advanceTimersByTimeAsync(80);
			}
			expect(start.mock.calls.map(([at]) => Math.round(at * 1000))).toEqual([
				1000, 1020, 1120, 1140, 1080
			]);
			expect(stop).toHaveBeenCalledTimes(2);
		} finally {
			playback.close();
			vi.unstubAllGlobals();
			vi.useRealTimers();
		}
	});
});
