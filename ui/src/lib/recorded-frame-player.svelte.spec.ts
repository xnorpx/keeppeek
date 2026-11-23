import { expect, test } from 'vitest';
import { RecordedFramePlayer } from './recorded-frame-player.svelte';
import { UnsupportedRecordedFrame, type PresentedStoredFrame } from './stored-frame-step';

function picture(ordinal: number): PresentedStoredFrame {
	const canvas = document.createElement('canvas');
	canvas.width = 2;
	canvas.height = 2;
	return {
		frame: new VideoFrame(canvas, { timestamp: ordinal }),
		sampleToken: new Uint8Array([ordinal]),
		presentationTimeNs: BigInt(ordinal) * 40_000_000n,
		storedTimeNs: BigInt(ordinal) * 41_000_000n,
		sourceTicks: BigInt(ordinal) * 40n,
		sourceTimescale: 1000
	};
}

test.each([
	{ label: 'forward', queued: Array<1 | -1>(40).fill(1), expected: Array<number>(32).fill(1) },
	{
		label: 'reverse then forward',
		queued: [...Array<1 | -1>(40).fill(-1), 1, 1] as const,
		expected: Array<number>(30).fill(-1)
	},
	{
		label: 'alternating',
		queued: Array.from({ length: 39 }, (_, i) => (i % 2 === 0 ? (-1 as const) : (1 as const))),
		expected: [-1]
	}
])(
	'coalesces $label repeats into at most 32 steps behind one active request',
	async ({ queued, expected }) => {
		const player = new RecordedFramePlayer();
		player.displayedNs = 345n;
		const first = Promise.withResolvers<void>();
		const directions: number[] = [];
		let active = 0;
		let peak = 0;
		const cursor = {
			canStepExactly: true,
			anchorTimeMs: 0,
			url: 'clip',
			async stepRecordedFrame(
				direction: -1 | 1,
				anchor: { displayedTimeNs: bigint } | { sampleToken: Uint8Array }
			) {
				active++;
				peak = Math.max(peak, active);
				directions.push(direction);
				const index = directions.length;
				if (index === 1) {
					expect(anchor).toEqual({ displayedTimeNs: 345n });
					await first.promise;
				} else {
					expect(anchor).toEqual({ sampleToken: new Uint8Array([index - 1]) });
				}
				active--;
				return picture(index);
			}
		};
		const displayed: VideoFrame[] = [];
		const present = (sample: PresentedStoredFrame) => {
			displayed.push(sample.frame);
		};
		const work = player.step(1, cursor, present);
		for (const direction of queued) await player.step(direction, cursor, present);
		expect(directions).toEqual([1]);
		first.resolve();
		await work;
		expect(peak).toBe(1);
		expect(directions).toEqual([1, ...expected]);
		expect(displayed).toHaveLength(expected.length + 1);
		expect(displayed.every((frame) => frame.codedWidth === 0)).toBe(true);
		expect(player.selection?.sourceTicks).toBe(BigInt(expected.length + 1) * 40n);
	}
);

test('a seek drops pending keys and closes a late decoder output', async () => {
	const player = new RecordedFramePlayer();
	player.displayedNs = 100n;
	const first = Promise.withResolvers<PresentedStoredFrame>();
	const cursor = {
		canStepExactly: true,
		anchorTimeMs: 0,
		url: 'clip',
		stepRecordedFrame: () => first.promise
	};
	let presented = 0;
	const present = () => {
		presented++;
	};
	const work = player.step(1, cursor, present);
	await player.step(-1, cursor, present);
	player.clear();
	const stale = picture(1);
	first.resolve(stale);
	await work;
	expect(presented).toBe(0);
	expect(stale.frame.codedWidth).toBe(0);
	expect(player.selection).toBeUndefined();
	expect(player.displayedNs).toBeUndefined();
	expect(player.busy).toBe(false);
});

test('unavailable source timing disables further exact steps until the cursor changes', async () => {
	const player = new RecordedFramePlayer();
	player.displayedNs = 0n;
	let attempts = 0;
	const cursor = {
		canStepExactly: true,
		anchorTimeMs: 0,
		url: 'legacy',
		async stepRecordedFrame() {
			attempts++;
			throw new UnsupportedRecordedFrame(
				'Original picture timing is unavailable for this recording.'
			);
		}
	};
	await player.step(1, cursor, () => {
		throw new Error('An unavailable picture was presented');
	});
	expect(player.unsupported).toBe(true);
	expect(player.notice).toContain('Original picture timing');
	await player.step(-1, cursor, () => {});
	expect(attempts).toBe(1);
	player.clear();
	expect(player.unsupported).toBe(false);
});

function observedVideo() {
	const video = document.createElement('video');
	let next = 0;
	const pending = new Map<number, VideoFrameRequestCallback>();
	let seeking = false;
	let position = 0;
	let url = 'blob:recording';
	Object.defineProperties(video, {
		currentSrc: { get: () => url },
		currentTime: { get: () => position },
		seeking: { get: () => seeking },
		requestVideoFrameCallback: {
			value: (cb: VideoFrameRequestCallback) => {
				pending.set(++next, cb);
				return next;
			}
		},
		cancelVideoFrameCallback: { value: (id: number) => pending.delete(id) }
	});
	return {
		video,
		callback() {
			const entry = [...pending.entries()][0];
			expect(entry).toBeDefined();
			pending.delete(entry[0]);
			return (mediaTime: number) => entry[1](0, { mediaTime } as VideoFrameCallbackMetadata);
		},
		seek(time: number) {
			seeking = true;
			position = time;
			video.dispatchEvent(new Event('seeking'));
		},
		settle() {
			seeking = false;
			video.dispatchEvent(new Event('seeked'));
		},
		replace() {
			url = 'blob:replacement';
		}
	};
}

test('uses a witnessed paused frame when the compositor callback precedes seeked', () => {
	const fixture = observedVideo();
	const player = new RecordedFramePlayer();
	const cleanup = player.follow(fixture.video, {
		url: fixture.video.currentSrc,
		anchorTimeMs: 1000,
		canStepExactly: true,
		stepRecordedFrame: () => Promise.resolve(picture(1))
	});
	fixture.callback()(0);
	expect(player.displayedNs).toBe(1_000_000_000n);
	const old = fixture.callback();
	fixture.seek(0.16);
	old(0.16);
	fixture.settle();
	expect(player.displayedNs).toBeUndefined();
	fixture.seek(0.16);
	fixture.callback()(0.16);
	expect(player.displayedNs).toBeUndefined();
	fixture.settle();
	expect(player.displayedNs).toBe(1_160_000_000n);
	fixture.seek(0.24);
	fixture.callback()(0.16);
	fixture.settle();
	expect(player.displayedNs).toBeUndefined();
	fixture.seek(0.32);
	fixture.callback()(0.32);
	fixture.replace();
	fixture.settle();
	expect(player.displayedNs).toBeUndefined();
	cleanup();
});
