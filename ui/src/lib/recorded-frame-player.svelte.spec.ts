import { expect, test } from 'vitest';
import { RecordedFramePlayer } from './recorded-frame-player.svelte';
import type { PresentedStoredFrame } from './stored-frame-step';

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

test('repeated keys queue up to 32 steps behind one active request and use each displayed sample', async () => {
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
	for (let i = 0; i < 39; i++) await player.step(i % 2 === 0 ? -1 : 1, cursor, present);
	expect(directions).toEqual([1]);
	first.resolve();
	await work;
	expect(peak).toBe(1);
	expect(directions).toEqual([1, ...Array.from({ length: 32 }, (_, i) => (i % 2 === 0 ? -1 : 1))]);
	expect(displayed).toHaveLength(33);
	expect(displayed.every((frame) => frame.codedWidth === 0)).toBe(true);
	expect(player.selection?.sourceTicks).toBe(1320n);
});

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
