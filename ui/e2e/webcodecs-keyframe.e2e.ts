import { readFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
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
const keyframeOffset = 897;
const keyframeLength = 11_688;
const decoderConfig = Buffer.from(
	'AULAH//hABhnQsAf2QCgL/lhAAADAAEAAAMAHg8YMkgBAAVoy4JLIA==',
	'base64'
);

test('decodes the exact indexed H.264 keyframe with WebCodecs metadata', async ({ page }) => {
	await page.goto('/');
	const recording = await readFile(h264FixturePath);
	const keyframe = recording.subarray(keyframeOffset, keyframeOffset + keyframeLength);
	expect(await decodeKeyframe(page, 'avc1.42C01F', 640, 368, decoderConfig, keyframe)).toBe(true);
});

test('decodes the exact indexed H.265 keyframe with WebCodecs metadata', async ({ page }) => {
	await page.goto('/');
	const recording = await readFile(h265FixturePath);
	const packet = probeFirstKeyPacket(h265FixturePath);
	const keyframe = recording.subarray(packet.offset, packet.offset + packet.length);
	const supported = await decodeKeyframe(
		page,
		'hvc1.1.6.L63.90',
		640,
		360,
		probeDecoderConfig(h265FixturePath),
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

function probeFirstKeyPacket(filePath: string): { offset: number; length: number } {
	const result = JSON.parse(
		execFileSync(
			'ffprobe',
			[
				'-v',
				'error',
				'-read_intervals',
				'%+#1',
				'-select_streams',
				'v:0',
				'-show_packets',
				'-show_entries',
				'packet=pos,size,flags',
				'-of',
				'json',
				filePath
			],
			{ encoding: 'utf8' }
		)
	) as { packets?: Array<{ pos?: string; size?: string; flags?: string }> };
	const packet = result.packets?.[0];
	if (!packet?.flags?.includes('K')) throw new Error('Fixture does not begin with a keyframe');
	const offset = Number(packet.pos);
	const length = Number(packet.size);
	if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(length) || length <= 0) {
		throw new Error('Fixture keyframe range is invalid');
	}
	return { offset, length };
}

function probeDecoderConfig(filePath: string): Uint8Array {
	const result = JSON.parse(
		execFileSync(
			'ffprobe',
			[
				'-v',
				'error',
				'-select_streams',
				'v:0',
				'-show_streams',
				'-show_data',
				'-of',
				'json',
				filePath
			],
			{ encoding: 'utf8' }
		)
	) as { streams?: Array<{ extradata?: string }> };
	const hex = (result.streams?.[0]?.extradata ?? '')
		.split('\n')
		.filter((line) => line.includes(':'))
		.map(
			(line) =>
				line
					.slice(line.indexOf(':') + 1)
					.split('  ')[0]
					?.replaceAll(' ', '') ?? ''
		)
		.join('');
	if (!hex || hex.length % 2 !== 0) throw new Error('Fixture decoder configuration is invalid');
	return Uint8Array.from(Buffer.from(hex, 'hex'));
}
