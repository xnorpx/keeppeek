import { expect, test } from 'vitest';
import { RecordedFramePlayer } from './recorded-frame-player.svelte';
import { UnsupportedRecordedFrame, type PresentedStoredFrame } from './stored-frame-step';

function clock(originMs: number) {
	const origin = BigInt(originMs) * 1_000_000n;
	return {
		storedNsForMediaUs: (us: number) => origin + BigInt(us) * 1_000n,
		mediaUsForStoredNs: (ns: bigint) => Math.round(Number(ns - origin) / 1_000)
	};
}

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
			...clock(0),
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
		...clock(0),
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
		...clock(0),
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
		currentTime: {
			get: () => position,
			set: (time: number) => {
				position = time;
			}
		},
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
		...clock(1000),
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

test('resuming waits for the selected compositor picture and a settled seek, and ignores cancelled work', async () => {
	const fixture = observedVideo();
	const player = new RecordedFramePlayer();
	const sample = picture(1);
	sample.frame.close();
	const cursor = {
		url: fixture.video.currentSrc,
		...clock(0),
		canStepExactly: true,
		stepRecordedFrame: () => Promise.resolve(picture(1))
	};
	player.selection = sample;
	const align = player.alignForPlay(fixture.video, cursor);
	fixture.seek(0.041001);
	fixture.callback()(0.03);
	fixture.callback()(0.041);
	expect(player.selection).toBeDefined();
	fixture.settle();
	expect(await align).toBe(true);
	expect(player.selection).toBeUndefined();
	player.selection = sample;
	const cancelled = player.alignForPlay(fixture.video, cursor);
	player.clear();
	expect(await cancelled).toBe(false);
});

test('probes recorded GOP intervals and exposes only the matching paused picture', async () => {
	const fixture = observedVideo();
	const player = new RecordedFramePlayer();
	const sample = picture(1);
	sample.frame.close();
	sample.resumeTimesNs = [41_001_000n, 80_001_000n, 119_900_000n];
	player.selection = sample;
	const url = fixture.video.currentSrc;
	const cursor = {
		url,
		...clock(0),
		canStepExactly: true,
		stepRecordedFrame: () => Promise.resolve(sample)
	};
	const aligning = player.alignForPlay(fixture.video, cursor);
	const stale = fixture.callback();
	stale(0.02);
	await Promise.resolve();
	expect(fixture.video.currentTime).toBeCloseTo(0.080001, 6);
	stale(0.041);
	expect(player.selection).toBeDefined();
	fixture.callback()(0.08);
	await Promise.resolve();
	expect(fixture.video.currentTime).toBeCloseTo(0.1199, 6);
	fixture.callback()(0.041);
	expect(await aligning).toBe(true);
	expect(player.selection).toBeUndefined();

	sample.resumeTimesNs = [];
	player.selection = sample;
	expect(await player.alignForPlay(fixture.video, cursor)).toBe(false);
	expect(player.selection).toBeDefined();
	expect(player.notice).toContain('could not resume');
});

test('a retained cold picture waits for the replacement source and verifies before the canvas is removed', async () => {
	const fixture = observedVideo();
	const player = new RecordedFramePlayer();
	const cursor = {
		url: 'blob:replacement',
		...clock(0),
		canStepExactly: true,
		stepRecordedFrame: () => Promise.resolve(picture(1))
	};
	let ready = false;
	Object.defineProperty(fixture.video, 'readyState', {
		get: () => (ready ? HTMLMediaElement.HAVE_CURRENT_DATA : HTMLMediaElement.HAVE_NOTHING)
	});
	const retained = {
		storedTimeNs: 41_000_000n,
		presentationTimeNs: 40_000_000n,
		resumeTimesNs: [41_001_000n]
	};
	let hidden = 0;
	const controller = new AbortController();
	const align = player.alignForPlay(fixture.video, cursor, {
		picture: retained,
		signal: controller.signal,
		verified: () => {
			hidden++;
			return true;
		}
	});
	expect(hidden).toBe(0);
	fixture.replace();
	ready = true;
	fixture.video.dispatchEvent(new Event('loadeddata'));
	await Promise.resolve();
	fixture.callback()(0.041);
	expect(await align).toBe(true);
	expect(hidden).toBe(1);
	expect(player.selection).toBeUndefined();
	ready = false;
	const abandoned = player.alignForPlay(fixture.video, cursor, {
		picture: retained,
		signal: controller.signal,
		verified: () => {
			hidden++;
			return true;
		}
	});
	controller.abort();
	fixture.video.dispatchEvent(new Event('loadeddata'));
	expect(await abandoned).toBe(false);
	expect(hidden).toBe(1);
});
