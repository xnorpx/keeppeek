import { expect, test } from 'vitest';
import { create } from '@bufbuild/protobuf';
import { timestampFromDate, durationFromMs } from '@bufbuild/protobuf/wkt';
import cameraFixture from '../../../tests/fixtures/exact_frame_step/fractional-origin.mp4?url';
import { StoredFrameClock } from './stored-frame-clock';
import { StoredMediaPlayback } from './control-client';
import {
	StoredMediaFragmentSchema,
	StoredMediaInitializationSchema,
	StoredMediaMode,
	StoredMediaStateSchema,
	StoredMediaStatus
} from './proto/webrtc_pb';

async function fractionalFixture() {
	const input = new Uint8Array(await (await fetch(cameraFixture)).arrayBuffer());
	const view = new DataView(input.buffer);
	let split = 0;
	while (split + 8 <= input.length && view.getUint32(split + 4) !== 0x6d6f6f66)
		split += view.getUint32(split);
	expect(split).toBeGreaterThan(0);
	expect(split).toBeLessThan(input.length);

	return { init: input.slice(0, split), fragment: input.slice(split) };
}

test('a paused multiplexed picture maps back to its fractional 90 kHz timestamp through real sequence MSE', async () => {
	const { init, fragment } = await fractionalFixture();

	const now = 1_700_000_000_000;
	const start = timestampFromDate(new Date(now + 1666));
	const mime = 'video/mp4; codecs="avc1.64000a,mp4a.40.2"';
	const state = create(StoredMediaStateSchema, {
		storedMediaId: 'cursor',
		generation: 1n,
		mode: StoredMediaMode.PLAYBACK,
		status: StoredMediaStatus.ENDED,
		requestedTime: start,
		fragmentTime: start,
		delivery: { contentType: mime }
	});
	const cursor = new StoredMediaPlayback(
		'cursor',
		'camera',
		'main',
		async () => state,
		async () => state,
		async () => state,
		async () => {},
		async () => {
			throw new Error('unused');
		}
	);
	const video = document.createElement('video');
	video.muted = true;
	document.body.append(video);
	try {
		const displayed = new Promise<number>((resolve) =>
			video.requestVideoFrameCallback((_, metadata) => resolve(metadata.mediaTime))
		);
		video.src = cursor.url;
		const common = {
			storedMediaId: cursor.id,
			generation: 1n,
			initializationId: 1n,
			chunkCount: 1
		};
		cursor.receiveInitialization(
			create(StoredMediaInitializationSchema, {
				...common,
				contentType: mime,
				payload: init
			})
		);
		cursor.receiveFragment(
			create(StoredMediaFragmentSchema, {
				...common,
				sequence: 1n,
				startTime: start,
				duration: durationFromMs(1000),
				payload: fragment
			})
		);
		cursor.configure(state);
		const us = Math.round((await displayed) * 1_000_000);
		expect(video.paused).toBe(true);
		expect(video.videoWidth).toBe(64);
		// AAC starts at tick 78,720/48,000; Chromium sets the sequence origin from it.
		expect(Math.abs(us - 26_666)).toBeLessThanOrEqual(1);
		const stored = BigInt(now) * 1_000_000n + 1_666_666_666n;
		const actual = cursor.storedNsForMediaUs(us);
		expect(actual).toBeDefined();
		expect(Math.abs(Number(actual! - stored))).toBeLessThan(1_000);
		expect(Math.abs(cursor.mediaUsForStoredNs(stored)! - us)).toBeLessThanOrEqual(1);
	} finally {
		cursor.dispose();
		video.remove();
	}
});

test('new recording periods retain distinct absolute video clocks until the cursor resets', async () => {
	const { init, fragment } = await fractionalFixture();
	const clock = new StoredFrameClock();
	clock.initialization(init);
	clock.fragment(fragment, 11666, -1.666666);
	expect(clock.storedNs(0)).toBe(11666666000n);
	clock.initialization(init);
	clock.fragment(fragment, 21666, -0.666666);
	expect(clock.storedNs(0)).toBe(11666666000n);
	expect(clock.storedNs(1000000)).toBe(21666666000n);
	expect(clock.mediaUs(11666666666n)).toBe(1);
	expect(clock.mediaUs(21666666666n)).toBe(1000001);
	expect(clock.mediaUs(15000000000n)).toBeUndefined();
	clock.reset();
	expect(clock.storedNs(0)).toBeUndefined();
});
