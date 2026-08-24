import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, test, type Page } from '@playwright/test';

const h264FixturePath = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	'../../crates/test-camera/testdata/cc-4k-640x360-h264.mp4'
);
const h265FixturePath = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	'../../crates/test-camera/testdata/cc-4k-640x360-h265.mp4'
);
const h264KeyframeOffset = 897;
const h264KeyframeLength = 11_688;
const h264DecoderConfig = Buffer.from(
	'AULAH//hABhnQsAf2QCgL/lhAAADAAEAAAMAHg8YMkgBAAVoy4JLIA==',
	'base64'
);
const h265KeyframeOffset = 3_277;
const h265KeyframeLength = 9_986;
const h265DecoderConfigOffset = 547;
const h265DecoderConfigLength = 2_414;
const mixedCodecFixtureRoot = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	'../../target/e2e-mixed-codec'
);

test('decodes the exact indexed H.264 keyframe with WebCodecs metadata', async ({ page }) => {
	await page.goto('/');
	const recording = await readFile(h264FixturePath);
	const keyframe = recording.subarray(h264KeyframeOffset, h264KeyframeOffset + h264KeyframeLength);
	expect(await decodeKeyframe(page, 'avc1.42C01F', 640, 368, h264DecoderConfig, keyframe)).toBe(
		true
	);
});

test('decodes the exact indexed H.265 keyframe with WebCodecs metadata', async ({ page }) => {
	await page.goto('/');
	const recording = await readFile(h265FixturePath);
	expect(recording.readUInt32BE(h265DecoderConfigOffset - 8)).toBe(h265DecoderConfigLength + 8);
	expect(recording.toString('ascii', h265DecoderConfigOffset - 4, h265DecoderConfigOffset)).toBe(
		'hvcC'
	);
	const decoderConfig = recording.subarray(
		h265DecoderConfigOffset,
		h265DecoderConfigOffset + h265DecoderConfigLength
	);
	const keyframe = recording.subarray(h265KeyframeOffset, h265KeyframeOffset + h265KeyframeLength);
	const supported = await decodeKeyframe(
		page,
		'hvc1.1.6.L63.90',
		640,
		360,
		decoderConfig,
		keyframe
	);
	test.skip(!supported, 'Bundled Chromium does not expose HEVC WebCodecs support');
});

test('plays H.264 to H.265 to H.264 fragmented MP4 periods through MSE', async ({ page }) => {
	test.setTimeout(20_000);
	await page.goto('/');
	const initial = splitFragmentedMp4(
		await readFile(path.join(mixedCodecFixtureRoot, 'h264-period.mp4'))
	);
	const h265 = splitFragmentedMp4(
		await readFile(path.join(mixedCodecFixtureRoot, 'h265-period.mp4'))
	);
	expect(initial.fragments).toHaveLength(1);
	expect(h265.fragments).toHaveLength(1);
	const result = await playMsePeriods(
		page,
		initial,
		h265,
		'video/mp4; codecs="avc1.42C01F"',
		'video/mp4; codecs="hev1.1.6.L63.90"'
	);
	test.skip(!result.supported, 'Bundled Chromium does not expose mixed AVC/HEVC MSE support');
	expect(result.frames).toBeGreaterThanOrEqual(3);
	expect(result.currentTime).toBeGreaterThanOrEqual(2.5);
});

test('plays low to high to low H.264 fragmented MP4 periods through MSE', async ({ page }) => {
	test.setTimeout(20_000);
	await page.goto('/');
	const initial = splitFragmentedMp4(
		await readFile(path.join(mixedCodecFixtureRoot, 'h264-period.mp4'))
	);
	const high = splitFragmentedMp4(
		await readFile(path.join(mixedCodecFixtureRoot, 'h264-high-period.mp4'))
	);
	const result = await playMsePeriods(
		page,
		initial,
		high,
		'video/mp4; codecs="avc1.42C01F"',
		'video/mp4; codecs="avc1.640033"'
	);
	expect(result.supported).toBe(true);
	expect(result.frames).toBeGreaterThanOrEqual(3);
	expect(result.currentTime).toBeGreaterThanOrEqual(2.5);
});

function splitFragmentedMp4(recording: Buffer): { initialization: Buffer; fragments: Buffer[] } {
	let offset = 0;
	let initializationEnd = -1;
	let fragmentStart = -1;
	const fragments: Buffer[] = [];
	while (offset < recording.length) {
		if (offset + 8 > recording.length) throw new Error('truncated top-level MP4 box');
		let boxSize = recording.readUInt32BE(offset);
		const boxType = recording.toString('ascii', offset + 4, offset + 8);
		if (boxSize === 1) {
			if (offset + 16 > recording.length) throw new Error('truncated large MP4 box');
			boxSize = Number(recording.readBigUInt64BE(offset + 8));
		} else if (boxSize === 0) {
			boxSize = recording.length - offset;
		}
		if (boxSize < 8 || offset + boxSize > recording.length) {
			throw new Error(`invalid ${boxType} box size`);
		}
		if (boxType === 'moof') {
			if (initializationEnd < 0) initializationEnd = offset;
			if (fragmentStart >= 0) fragments.push(recording.subarray(fragmentStart, offset));
			fragmentStart = offset;
		}
		offset += boxSize;
	}
	if (initializationEnd < 0 || fragmentStart < 0) throw new Error('fixture has no MP4 fragments');
	fragments.push(recording.subarray(fragmentStart));
	return { initialization: recording.subarray(0, initializationEnd), fragments };
}

async function playMsePeriods(
	page: Page,
	initial: { initialization: Buffer; fragments: Buffer[] },
	next: { initialization: Buffer; fragments: Buffer[] },
	initialType: string,
	nextType: string
): Promise<{ supported: boolean; frames: number; currentTime: number }> {
	return page.evaluate(
		async ({ initial, next, initialType, nextType }) => {
			if (
				typeof MediaSource === 'undefined' ||
				!MediaSource.isTypeSupported(initialType) ||
				!MediaSource.isTypeSupported(nextType)
			) {
				return { supported: false, frames: 0, currentTime: 0 };
			}

			const video = document.createElement('video');
			video.muted = true;
			video.playsInline = true;
			document.body.append(video);
			const mediaSource = new MediaSource();
			video.src = URL.createObjectURL(mediaSource);
			await new Promise<void>((resolve) =>
				mediaSource.addEventListener('sourceopen', () => resolve(), { once: true })
			);
			const sourceBuffer = mediaSource.addSourceBuffer(initialType);
			sourceBuffer.mode = 'sequence';
			const append = (label: string, bytes: number[]) =>
				new Promise<void>((resolve, reject) => {
					const onError = () => reject(new Error(`SourceBuffer rejected ${label}`));
					sourceBuffer.addEventListener('error', onError, { once: true });
					sourceBuffer.addEventListener(
						'updateend',
						() => {
							sourceBuffer.removeEventListener('error', onError);
							resolve();
						},
						{ once: true }
					);
					sourceBuffer.appendBuffer(Uint8Array.from(bytes));
				});

			await append('initial initialization', initial.initialization);
			for (const [index, fragment] of initial.fragments.entries()) {
				await append(`initial fragment ${index + 1}`, fragment);
			}
			sourceBuffer.changeType(nextType);
			await append('updated initialization', next.initialization);
			for (const [index, fragment] of next.fragments.entries()) {
				await append(`updated fragment ${index + 1}`, fragment);
			}
			sourceBuffer.changeType(initialType);
			await append('return initialization', initial.initialization);
			for (const [index, fragment] of initial.fragments.entries()) {
				await append(`return fragment ${index + 1}`, fragment);
			}
			mediaSource.endOfStream();
			await video.play();
			await Promise.race([
				new Promise<void>((resolve) =>
					video.addEventListener('ended', () => resolve(), { once: true })
				),
				new Promise<never>((_, reject) =>
					setTimeout(() => reject(new Error('MSE transition playback timed out')), 8_000)
				)
			]);
			const frames = video.getVideoPlaybackQuality().totalVideoFrames;
			const currentTime = video.currentTime;
			URL.revokeObjectURL(video.src);
			video.remove();
			return { supported: true, frames, currentTime };
		},
		{
			initial: {
				initialization: [...initial.initialization],
				fragments: initial.fragments.map((fragment) => [...fragment])
			},
			next: {
				initialization: [...next.initialization],
				fragments: next.fragments.map((fragment) => [...fragment])
			},
			initialType,
			nextType
		}
	);
}

async function decodeKeyframe(
	page: Page,
	codec: string,
	codedWidth: number,
	codedHeight: number,
	description: Uint8Array,
	data: Uint8Array
): Promise<boolean> {
	const decoded = await page.evaluate(
		async ({ codec, codedWidth, codedHeight, description, data }) => {
			if (typeof VideoDecoder === 'undefined')
				throw new Error('WebCodecs VideoDecoder is unavailable');
			const config: VideoDecoderConfig = {
				codec,
				codedWidth,
				codedHeight,
				description: Uint8Array.from(description)
			};
			const support = await VideoDecoder.isConfigSupported(config);
			if (!support.supported) return { supported: false, frames: [] };
			const frames: Array<{ displayWidth: number; displayHeight: number }> = [];
			let decoderError: Error | undefined;
			const decoder = new VideoDecoder({
				output(frame) {
					frames.push({ displayWidth: frame.displayWidth, displayHeight: frame.displayHeight });
					frame.close();
				},
				error(error) {
					decoderError = error;
				}
			});
			decoder.configure(config);
			decoder.decode(
				new EncodedVideoChunk({
					type: 'key',
					timestamp: 0,
					duration: 66_667,
					data: Uint8Array.from(data)
				})
			);
			await decoder.flush();
			decoder.close();
			if (decoderError) throw decoderError;
			return { supported: true, frames };
		},
		{
			codec,
			codedWidth,
			codedHeight,
			description: [...description],
			data: [...data]
		}
	);

	if (!decoded.supported) return false;
	expect(decoded.frames).toEqual([{ displayWidth: 640, displayHeight: 360 }]);
	return true;
}
