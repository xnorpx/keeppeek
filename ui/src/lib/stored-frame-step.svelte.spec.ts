import { create } from '@bufbuild/protobuf';
import { expect, onTestFinished, test, vi } from 'vitest';
import fixtureUrl from '../../../tests/fixtures/exact_frame_step/vfr.mp4?inline';
import { StoredFrameWindow } from './stored-frame-step';
import {
	StoredFrameStepChunkSchema,
	StoredFrameStepResultSchema,
	StoredFrameStepStatus
} from './proto/webrtc_pb';

// FFprobe packet extents from the first GOP of the numbered VFR fixture, in decode order.
const packets = [
	{ start: 960, size: 667, rank: 0 },
	{ start: 1627, size: 25, rank: 3 },
	{ start: 1652, size: 16, rank: 1 },
	{ start: 1668, size: 16, rank: 2 },
	{ start: 1684, size: 23, rank: 5 },
	{ start: 1707, size: 17, rank: 4 }
];

test('Chromium displays the numbered B pictures selected from the recorded decode window', async () => {
	const frames = new Map<VideoFrame, number>();
	let peakFrames = 0;
	let peakOutputBytes = 0;
	let activeDecoders = 0;
	let peakDecoders = 0;
	const closeFrame = VideoFrame.prototype.close;
	const frameClose = vi.spyOn(VideoFrame.prototype, 'close').mockImplementation(function (
		this: VideoFrame
	) {
		frames.delete(this);
		closeFrame.call(this);
	});
	const NativeVideoDecoder = VideoDecoder;
	class ObservedDecoder extends NativeVideoDecoder {
		constructor(init: VideoDecoderInit) {
			super({
				error: init.error,
				output(frame) {
					frames.set(frame, frame.allocationSize());
					peakFrames = Math.max(peakFrames, frames.size);
					peakOutputBytes = Math.max(
						peakOutputBytes,
						[...frames.values()].reduce((total, bytes) => total + bytes, 0)
					);
					init.output(frame);
				}
			});
			peakDecoders = Math.max(peakDecoders, ++activeDecoders);
		}

		override close() {
			super.close();
			activeDecoders--;
		}
	}

	vi.stubGlobal('VideoDecoder', ObservedDecoder);
	onTestFinished(() => {
		frameClose.mockRestore();
		vi.unstubAllGlobals();
	});

	const file = new Uint8Array(await (await fetch(fixtureUrl)).arrayBuffer());
	const type = new TextDecoder('latin1').decode(file).indexOf('avcC');
	expect(type).toBeGreaterThan(3);
	const size = new DataView(file.buffer).getUint32(type - 4);
	const decoderConfig = file.slice(type + 4, type - 4 + size);
	const times = [40, 80, 120, 240, 280, 320];
	const canvas = document.createElement('canvas');
	canvas.width = 64;
	canvas.height = 32;
	const context = canvas.getContext('2d', { willReadFrequently: true })!;
	for (const target of [0, 1, 2, 3, 4, 5, 4, 3, 2, 1, 0]) {
		const identity = { storedMediaId: 'cursor', generation: 1n, stepId: crypto.randomUUID() };
		const window = new StoredFrameWindow(
			identity.storedMediaId,
			identity.generation,
			identity.stepId
		);
		const result = create(StoredFrameStepResultSchema, {
			...identity,
			status: StoredFrameStepStatus.READY,
			sampleToken: new Uint8Array([target]),
			presentationTime: { nanos: times[target] * 1e6 },
			storedTime: { seconds: 2n, nanos: target * 40_000_000 },
			sourceTicks: BigInt(times[target]),
			sourceTimescale: 1_000,
			targetPresentationRank: target,
			sampleCount: packets.length,
			encodedBytes: packets.reduce((sum, packet) => sum + packet.size, 0),
			configuration: {
				codec: { name: 'avc1.64000a' },
				format: { format: { case: 'video', value: { width: 64, height: 32, decoderConfig } } }
			}
		});
		const decode = window.decode(result, AbortSignal.timeout(5_000));
		for (const [decodeIndex, packet] of packets.entries()) {
			window.receive(
				create(StoredFrameStepChunkSchema, {
					...identity,
					decodeIndex,
					presentationRank: packet.rank,
					randomAccess: decodeIndex === 0,
					chunkCount: 1,
					payload: file.slice(packet.start, packet.start + packet.size),
					sampleBytes: packet.size
				})
			);
		}
		const sample = await decode;
		try {
			context.drawImage(sample.frame, 0, 0);
			let picture = 0;
			for (let bit = 0; bit < 4; bit++) {
				if (context.getImageData(8 + 16 * bit, 16, 1, 1).data[0] > 128) picture |= 1 << bit;
			}
			expect(picture).toBe(target);
			expect(sample.presentationTimeNs).toBe(BigInt(times[target]) * 1_000_000n);
		} finally {
			sample.frame.close();
		}
	}
	expect(peakDecoders).toBe(1);
	expect(peakFrames).toBe(2);
	expect(frames.size).toBe(0);
	expect(activeDecoders).toBe(0);
	expect(peakOutputBytes).toBeGreaterThan(0);
	expect(peakOutputBytes).toBeLessThanOrEqual(2 * canvas.width * canvas.height * 4);
	console.info('Recorded step decoder resources', { peakDecoders, peakFrames, peakOutputBytes });
});
