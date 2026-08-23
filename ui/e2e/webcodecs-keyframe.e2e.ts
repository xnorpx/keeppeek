import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, test } from '@playwright/test';

const fixturePath = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	'../../crates/test-camera/testdata/cc-4k-640x360-h264.mp4'
);
const keyframeOffset = 897;
const keyframeLength = 11_688;
const decoderConfig = Buffer.from(
	'AULAH//hABhnQsAf2QCgL/lhAAADAAEAAAMAHg8YMkgBAAVoy4JLIA==',
	'base64'
);

test('decodes the exact indexed H.264 keyframe with WebCodecs metadata', async ({ page }) => {
	await page.goto('/');
	const recording = await readFile(fixturePath);
	const keyframe = recording.subarray(keyframeOffset, keyframeOffset + keyframeLength);

	const decoded = await page.evaluate(
		async ({ description, data }) => {
			if (typeof VideoDecoder === 'undefined')
				throw new Error('WebCodecs VideoDecoder is unavailable');
			const config: VideoDecoderConfig = {
				codec: 'avc1.42C01F',
				codedWidth: 640,
				codedHeight: 368,
				description: Uint8Array.from(description)
			};
			const support = await VideoDecoder.isConfigSupported(config);
			if (!support.supported) throw new Error('Chromium rejected the indexed H.264 configuration');
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
			return frames;
		},
		{
			description: [...decoderConfig],
			data: [...keyframe]
		}
	);

	expect(decoded).toEqual([{ displayWidth: 640, displayHeight: 360 }]);
});
