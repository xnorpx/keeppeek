import { expect, test } from '@playwright/test';
import { resolve } from 'node:path';
import type { RecordingCoverageResponse } from '../src/lib/types';
import { cardSourceIds, startHomeAssistantServer } from './fixtures/home-assistant-server';
import { recordedAudioBackendRoute } from './fixtures/recorded-audio-playback';

function observePicture(element: Element, expected: number | null) {
	if (!(element instanceof HTMLCanvasElement || element instanceof HTMLVideoElement))
		throw new Error('Expected a recorded picture surface.');
	const canvas = document.createElement('canvas');
	canvas.width = 64;
	canvas.height = 32;
	const context = canvas.getContext('2d', { willReadFrequently: true });
	if (!context) throw new Error('No canvas context for the picture oracle.');
	const read = () => {
		context.drawImage(element, 0, 0, 64, 32);
		let id = 0;
		for (let bit = 0; bit < 4; bit++) {
			if (context.getImageData(8 + 16 * bit, 16, 1, 1).data[0] > 128) id |= 1 << bit;
		}
		return id;
	};
	if (expected === null) return { id: read(), elapsedMs: 0 };
	return new Promise<{ id: number; elapsedMs: number }>((resolve, reject) => {
		const deadline = performance.now() + 8_000;
		const sample = () => {
			const now = performance.now();
			const id = read();
			const input = performance.getEntriesByName('frame-step-click')[0];
			if (input && id === expected && element.getClientRects().length > 0) {
				resolve({ id, elapsedMs: now - input.startTime });
			} else if (now >= deadline) {
				reject(new Error(`Expected picture ${expected}, last observed ${id}`));
			} else {
				requestAnimationFrame(sample);
			}
		};
		requestAnimationFrame(sample);
	});
}

for (const fixture of ['cfr', 'vfr', 'duplicate-pts']) {
	test(`steps recorded ${fixture} RTSP pictures in display order in both directions`, async ({
		page,
		context,
		request,
		baseURL
	}, testInfo) => {
		test.setTimeout(120_000);
		if (!baseURL) throw new Error('The frame-step fixture needs the UI origin.');
		const origin = new URL(baseURL).origin;
		const mediaPath = resolve(
			import.meta.dirname,
			`../../tests/fixtures/exact_frame_step/${fixture}.mp4`
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
			const measurements = [];
			let { id } = await video.evaluate(observePicture, null);
			for (const direction of [...Array<number>(12).fill(1), ...Array<number>(12).fill(-1)]) {
				const button = direction > 0 ? next : previous;
				await button.evaluate((element) => {
					performance.clearMarks('frame-step-click');
					element.addEventListener('click', () => performance.mark('frame-step-click'), {
						once: true,
						capture: true
					});
				});
				id = (id + direction + 12) % 12;
				const [presented] = await Promise.all([
					selected.evaluate(observePicture, id),
					button.click()
				]);
				expect(presented.id).toBe(id);
				measurements.push({ direction, picture: id, elapsedMs: presented.elapsedMs });
			}
			await testInfo.attach('input-to-recorded-picture', {
				body: JSON.stringify({ fixture, measurements }),
				contentType: 'application/json'
			});
		} finally {
			await recorder.close();
		}
	});
}
