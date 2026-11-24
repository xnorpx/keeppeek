import { expect, test, vi } from 'vitest';
import numberedVideo from '../../../tests/fixtures/exact_frame_step/vfr.mp4?url';
import { RecordedFramePlayer } from './recorded-frame-player.svelte';
import { UnsupportedRecordedFrame, type PresentedStoredFrame } from './stored-frame-step';

function clock(originMs: number) {
	const origin = BigInt(originMs) * 1_000_000n;
	return {
		onFrameClockUpdate: (_listener: () => void) => () => {},
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

test('anchors the first step to the current paused picture when a compositor callback lags', async () => {
	const player = new RecordedFramePlayer();
	const video = document.createElement('video');
	const url = URL.createObjectURL(await (await fetch(numberedVideo)).blob());
	const stepRecordedFrame = vi.fn(async () => picture(1));
	const cursor = { ...clock(1_000), url, canStepExactly: true, stepRecordedFrame };
	let cleanup: (() => void) | undefined;
	document.body.append(video);
	try {
		const loaded = new Promise<void>((resolve) =>
			video.addEventListener('loadeddata', () => resolve(), { once: true })
		);
		video.src = url;
		await loaded;
		const settled = new Promise<void>((resolve) =>
			video.addEventListener('seeked', () => resolve(), { once: true })
		);
		video.currentTime = 0.255;
		await settled;
		const frame = new VideoFrame(video);
		const currentUs = frame.timestamp;
		frame.close();
		expect(currentUs).toBeGreaterThan(0);
		expect(currentUs).not.toBe(Math.round(video.currentTime * 1_000_000));
		const observe = vi.spyOn(video, 'requestVideoFrameCallback').mockReturnValue(1);
		cleanup = player.follow(video, cursor);
		observe.mock.calls[0][0](0, { mediaTime: 0 } as VideoFrameCallbackMetadata);
		expect(player.displayedNs).toBe(1_000_000_000n);

		await player.step(1, cursor, () => {});
		expect(stepRecordedFrame).toHaveBeenCalledWith(
			1,
			{ displayedTimeNs: cursor.storedNsForMediaUs(currentUs) },
			expect.any(AbortSignal)
		);

		player.clear();
		observe.mock.calls[0][0](0, { mediaTime: 0 } as VideoFrameCallbackMetadata);
		await player.step(-1, { ...cursor, url: 'replacement' }, () => {});
		expect(stepRecordedFrame).toHaveBeenCalledTimes(1);
		expect(player.notice).toContain('paused picture is unavailable');
	} finally {
		cleanup?.();
		video.remove();
		URL.revokeObjectURL(url);
		vi.restoreAllMocks();
	}
});

test('anchors an already paused decoded picture when its source clock becomes available', async () => {
	const video = document.createElement('video');
	const url = URL.createObjectURL(await (await fetch(numberedVideo)).blob());
	const player = new RecordedFramePlayer();
	const notify = new Set<() => void>();
	let mapped = false;
	const cursor = {
		...clock(1_000),
		url,
		canStepExactly: true,
		onFrameClockUpdate(listener: () => void) {
			notify.add(listener);
			return () => notify.delete(listener);
		},
		storedNsForMediaUs: (us: number) => (mapped ? 1_000_000_000n + BigInt(us) * 1_000n : undefined),
		stepRecordedFrame: vi.fn(async () => picture(1))
	};
	let cleanup: (() => void) | undefined;
	document.body.append(video);
	try {
		const loaded = new Promise<void>((resolve) =>
			video.addEventListener('loadeddata', () => resolve(), { once: true })
		);
		video.src = url;
		await loaded;
		const settled = new Promise<void>((resolve) =>
			video.addEventListener('seeked', () => resolve(), { once: true })
		);
		video.currentTime = 0.255;
		await settled;
		const videoFrame = async (video: HTMLVideoElement) => {
			for (let i = 0; i < 20; i++) {
				if (video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
					try {
						return new VideoFrame(video);
					} catch {
						// VideoFrame construction can briefly race with Chromium decoder readiness.
					}
				}
				await new Promise((resolve) => setTimeout(resolve, 20));
			}
			return new VideoFrame(video);
		};
		const bitmap = await videoFrame(video);
		const us = bitmap.timestamp;
		bitmap.close();
		expect(us).not.toBe(Math.round(video.currentTime * 1_000_000));
		// Chromium can keep showing this paused picture without another compositor callback.
		vi.spyOn(video, 'requestVideoFrameCallback').mockReturnValue(1);
		cleanup = player.follow(video, cursor);
		expect(player.displayedNs).toBeUndefined();
		mapped = true;
		for (const listener of notify) listener();
		expect(player.displayedNs).toBe(cursor.storedNsForMediaUs(us));
		await player.step(1, cursor, () => {});
		expect(cursor.stepRecordedFrame).toHaveBeenCalledWith(
			1,
			{ displayedTimeNs: cursor.storedNsForMediaUs(us) },
			expect.any(AbortSignal)
		);
		cleanup();
		cleanup = undefined;
		expect(notify.size).toBe(0);
	} finally {
		cleanup?.();
		video.remove();
		URL.revokeObjectURL(url);
		vi.restoreAllMocks();
	}
});

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

test.each(['pause', 'timeout'] as const)(
	'a suspended paint releases the %s handoff without discarding the selected picture',
	async (ending) => {
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
		Object.defineProperty(fixture.video, 'readyState', {
			value: HTMLMediaElement.HAVE_CURRENT_DATA
		});
		const retained = new AbortController();
		const verified = vi.fn(() => true);
		if (ending === 'pause') player.selection = sample;
		const firstPaint = Promise.withResolvers<void>();
		const paints: FrameRequestCallback[] = [];
		if (ending === 'timeout') vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		const schedule = vi.spyOn(window, 'requestAnimationFrame').mockImplementation((cb) => {
			paints.push(cb);
			firstPaint.resolve();
			return paints.length;
		});
		const cancelled = vi.spyOn(window, 'cancelAnimationFrame').mockImplementation(() => {});
		try {
			const result = player.alignForPlay(
				fixture.video,
				cursor,
				ending === 'timeout' ? { picture: sample, signal: retained.signal, verified } : undefined
			);
			await Promise.resolve();
			fixture.callback()(0.041);
			await firstPaint.promise;
			expect(player.revealingPicture).toBe(true);
			expect(player.resumingPlayback).toBe(true);
			paints[0](performance.now());
			expect(paints).toHaveLength(2);
			if (ending === 'pause') player.cancelHandoff();
			else await vi.advanceTimersByTimeAsync(5_001);
			expect(await result).toBe(false);
			expect(cancelled).toHaveBeenCalledWith(2);
			paints[1](performance.now());
			expect(verified).not.toHaveBeenCalled();
			expect(player.revealingPicture).toBe(false);
			expect(player.resumingPlayback).toBe(false);
			expect(fixture.video.paused).toBe(true);
			if (ending === 'pause') expect(player.selection).toBe(sample);
			else expect(player.notice).toContain('could not resume');
		} finally {
			schedule.mockRestore();
			cancelled.mockRestore();
			if (ending === 'timeout') vi.useRealTimers();
		}
	}
);

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

test.each([false, true])(
	'a playing decoder can witness a missing paused picture and respects cancellation (%s)',
	async (cancel) => {
		const fixture = observedVideo();
		const player = new RecordedFramePlayer();
		const sample = picture(1);
		sample.frame.close();
		sample.resumeTimesNs = [41_001_000n, 1_000n];
		const cursor = {
			url: fixture.video.currentSrc,
			...clock(0),
			canStepExactly: true,
			stepRecordedFrame: () => Promise.resolve(picture(1))
		};
		let paused = true;
		Object.defineProperties(fixture.video, {
			paused: { get: () => paused },
			play: {
				value: async () => {
					paused = false;
				}
			},
			pause: {
				value: () => {
					paused = true;
				}
			}
		});
		fixture.video.playbackRate = 1.5;
		player.selection = sample;
		const alignment = player.alignForPlay(fixture.video, cursor);
		fixture.callback()(0.08);
		await Promise.resolve();
		fixture.callback()(0);
		await Promise.resolve();
		expect(player.preparingPlayback).toBe(true);
		fixture.settle();
		expect(fixture.video.paused).toBe(false);
		expect(fixture.video.muted).toBe(true);
		fixture.callback()(0.02);
		expect(player.selection).toBeDefined();
		if (cancel) player.cancelHandoff();
		else fixture.callback()(0.041);
		expect(await alignment).toBe(!cancel);
		expect(player.selection === undefined).toBe(!cancel);
		expect(fixture.video.paused).toBe(true);
		expect(fixture.video.muted).toBe(false);
		expect(fixture.video.playbackRate).toBe(1.5);
		expect(player.preparingPlayback).toBe(false);
	}
);

async function verifyReplacement(order: 'clock after data' | 'data recovery after clock') {
	const fixture = observedVideo();
	const player = new RecordedFramePlayer();
	let publishClock: (() => void) | undefined;
	let clockReady = false;
	const cursor = {
		url: 'blob:replacement',
		...clock(0),
		mediaUsForStoredNs: (ns: bigint) => (clockReady ? clock(0).mediaUsForStoredNs(ns) : undefined),
		onFrameClockUpdate: (listener: () => void) => {
			publishClock = listener;
			return () => {
				publishClock = undefined;
			};
		},
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
	expect(hidden).toBe(0);
	expect(publishClock).toBeDefined();
	// A cold seek may lose current data after loadeddata and settle before the decoder recovers.
	if (order === 'data recovery after clock') ready = false;
	clockReady = true;
	publishClock?.();
	if (order === 'data recovery after clock') {
		fixture.settle();
		expect(publishClock).toBeDefined();
		ready = true;
		fixture.video.dispatchEvent(new Event('canplay'));
	}
	await Promise.resolve();
	expect(publishClock).toBeUndefined();
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
}

test.each(['clock after data', 'data recovery after clock'] as const)(
	'a cold replacement waits for %s and verifies before removing the canvas',
	verifyReplacement
);
