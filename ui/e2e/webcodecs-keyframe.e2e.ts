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
