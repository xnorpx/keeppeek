import { describe, expect, it, vi } from 'vitest';
import { AacAudioPlayback } from './audio-playback';
import { create } from '@bufbuild/protobuf';
import { AudioDataFrameSchema } from './proto/webrtc_pb';

function installAacPlaybackClock() {
	const clock = { now: 1 };
	const nodes: Array<{ onended: (() => void) | null }> = [];
	const start = vi.fn();
	const stop = vi.fn();
	const disconnect = vi.fn();
	vi.stubGlobal(
		'AudioContext',
		class {
			get currentTime() {
				return clock.now;
			}
			createBuffer() {
				return { duration: 0.02, getChannelData: () => new Float32Array(1) };
			}
			createBufferSource() {
				const node = { connect: vi.fn(), disconnect, start, stop, onended: null };
				nodes.push(node);
				return node;
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
	return { start, stop, disconnect, clock, nodes };
}

function pushTimestamp(player: AacAudioPlayback, frame: number, ms: number) {
	player.push(
		create(AudioDataFrameSchema, {
			frameId: BigInt(frame),
			timestamp: { seconds: 10n + BigInt(Math.floor(ms / 1000)), nanos: (ms % 1000) * 1_000_000 },
			duration: { nanos: 20_000_000 },
			payload: new Uint8Array([1])
		})
	);
}

describe('reliable AAC presentation', () => {
	it('schedules contiguous late blocks in order after a short delivery stall', async () => {
		vi.useFakeTimers();
		const { clock, start, stop } = installAacPlaybackClock();
		const player = new AacAudioPlayback(new AudioContext());
		try {
			await player.configure('aac', new Uint8Array([0x12, 0x10]), 48_000, 1);
			pushTimestamp(player, 1, 0);
			await vi.advanceTimersByTimeAsync(80);
			clock.now = 1.24;
			for (let frame = 2; frame <= 5; frame++) pushTimestamp(player, frame, (frame - 1) * 20);
			await vi.advanceTimersByTimeAsync(12);
			expect(start.mock.calls.map(([time]) => Math.round(time * 1000))).toEqual([
				1000, 1240, 1260, 1280, 1300
			]);
			expect(stop).not.toHaveBeenCalled();
		} finally {
			player.close();
			vi.unstubAllGlobals();
			vi.useRealTimers();
		}
	});

	it('retires cancelled nodes immediately even if a suspended audio clock never emits ended', async () => {
		vi.useFakeTimers();
		const { nodes, stop } = installAacPlaybackClock();
		const player = new AacAudioPlayback(new AudioContext());
		try {
			await player.configure('aac', new Uint8Array([0x12, 0x10]), 48_000, 1);
			for (let frame = 0; frame < 128; frame++) {
				pushTimestamp(player, frame + 1, frame * 20);
				await vi.advanceTimersByTimeAsync(frame === 0 ? 80 : 2);
			}
			expect(nodes.length).toBe(128);
			expect(stop).toHaveBeenCalled();
			expect(nodes.filter((node) => node.onended !== null).length).toBeLessThanOrEqual(32);
		} finally {
			player.close();
			vi.unstubAllGlobals();
			vi.useRealTimers();
		}
	});

	it('silences scheduled output from a retired binding while its shared context stays available', async () => {
		vi.useFakeTimers();
		const { start, stop, disconnect } = installAacPlaybackClock();
		const context = new AudioContext();
		const closeContext = vi.spyOn(context, 'close');
		const player = new AacAudioPlayback(context);
		try {
			await player.configure('aac', new Uint8Array([0x12, 0x10]), 48_000, 1);
			pushTimestamp(player, 1, 0);
			pushTimestamp(player, 2, 20);
			await vi.advanceTimersByTimeAsync(90);
			expect(start).toHaveBeenCalledTimes(2);
			player.close();
			expect(stop).toHaveBeenCalledTimes(2);
			expect(disconnect).toHaveBeenCalledTimes(2);
			expect(closeContext).not.toHaveBeenCalled();
		} finally {
			await context.close();
			vi.unstubAllGlobals();
			vi.useRealTimers();
		}
	});

	it('applies changes to saved timing while earlier decoded audio is still buffered', async () => {
		vi.useFakeTimers();
		const { start, stop } = installAacPlaybackClock();
		const playback = new AacAudioPlayback(new AudioContext());
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
