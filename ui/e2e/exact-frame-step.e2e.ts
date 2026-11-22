import { expect, test, type Locator } from '@playwright/test';
import { resolve } from 'node:path';
import type { RecordingCoverageResponse } from '../src/lib/types';
import { cardSourceIds, startHomeAssistantServer } from './fixtures/home-assistant-server';
import { recordedAudioBackendRoute } from './fixtures/recorded-audio-playback';

async function displayedPicture(surface: Locator) {
	return surface.evaluate((element) => {
		if (!(element instanceof HTMLVideoElement || element instanceof HTMLCanvasElement)) {
			throw new Error('Expected the recorded video or its selected frame.');
		}
		const canvas = document.createElement('canvas');
		canvas.width = 64;
		canvas.height = 32;
		const context = canvas.getContext('2d', { willReadFrequently: true });
		if (!context) throw new Error('No canvas context for the picture oracle.');
		context.drawImage(element, 0, 0, 64, 32);
		let id = 0;
		for (let bit = 0; bit < 4; bit++) {
			if (context.getImageData(8 + 16 * bit, 16, 1, 1).data[0] > 128) id |= 1 << bit;
		}
		return id;
	});
}

test('steps the recorded RTSP pictures in display order through a duplicate timestamp', async ({
	page,
	context,
	request,
	baseURL
}) => {
	test.setTimeout(120_000);
	if (!baseURL) throw new Error('The frame-step fixture needs the UI origin.');
	const origin = new URL(baseURL).origin;
	const mediaPath = resolve(
		import.meta.dirname,
		'../../tests/fixtures/exact_frame_step/duplicate-pts.mp4'
	);
	const recorder = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		frameStepMedia: mediaPath
	});
	try {
		await context.route('**/*', recordedAudioBackendRoute(origin, recorder.url));
		const source = cardSourceIds[0];
		let start = 0;
		await expect
			.poll(
				async () => {
					const response = await request.get(`${recorder.url}/recording-coverage`, {
						params: { search: source, stream: 'sub' }
					});
					if (!response.ok()) return false;
					const coverage: RecordingCoverageResponse = await response.json();
					const camera = coverage.cameras.find((row) => row.camera_id === source);
					const video = camera?.streams.find((row) => row.stream_id === 'sub');
					const range = video?.ranges.find((row) => row.end_ms - row.start_ms >= 4_000);
					if (!range) return false;
					start = range.start_ms + 1_000;
					return true;
				},
				{ timeout: 60_000, intervals: [500, 1_000] }
			)
			.toBe(true);
		await page.goto(
			`/keep?camera=${encodeURIComponent(source)}&stream=sub&at=${start}&date=${new Date(start).toISOString().slice(0, 10)}`
		);
		const region = page.getByRole('region', { name: 'Recorded video player' });
		const video = region.locator('video');
		await expect
			.poll(() => video.evaluate((v) => (v instanceof HTMLVideoElement ? v.videoWidth : 0)))
			.toBe(64);
		const pause = page.getByRole('button', { name: 'Pause recording', exact: true });
		if (await pause.isVisible()) await pause.click();
		const next = page.getByRole('button', { name: 'Next recorded frame', exact: true });
		const previous = page.getByRole('button', { name: 'Previous recorded frame', exact: true });
		const selected = region.locator('canvas[aria-label="Selected recorded picture"]');
		await expect(next).toBeEnabled();
		let id = await displayedPicture(video);
		for (const direction of [...Array<number>(12).fill(1), -1, -1, -1]) {
			await (direction > 0 ? next : previous).click();
			id = (id + direction + 12) % 12;
			await expect(selected).toBeVisible();
			await expect.poll(() => displayedPicture(selected)).toBe(id);
		}
	} finally {
		await recorder.close();
	}
});
