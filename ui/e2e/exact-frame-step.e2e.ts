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

function observePlaybackPresentation() {
	document.addEventListener(
		'loadedmetadata',
		(event) => {
			const element = event.target;
			if (!(element instanceof HTMLVideoElement)) return;
			const trace: string[] = [];
			const record = (label: string) => {
				trace.push(label);
				if (trace.length > 16) trace.shift();
				element.dataset.presentationHistory = JSON.stringify(trace);
			};
			for (const type of ['seeking', 'seeked', 'pause', 'play']) {
				element.addEventListener(type, () => record(type + ' ' + element.currentTime.toFixed(6)));
			}
			const frame: VideoFrameRequestCallback = (_time, metadata) => {
				element.dataset.presentedPictures = String(metadata.presentedFrames);
				element.dataset.lastMediaTime = String(metadata.mediaTime);
				record('frame ' + metadata.mediaTime.toFixed(6) + ' seeking=' + element.seeking);
				element.requestVideoFrameCallback(frame);
			};
			element.requestVideoFrameCallback(frame);
		},
		true
	);
}

function witnessHandoff(surface: Element) {
	const region = surface.closest('[aria-label="Recorded video player"]');
	if (!(surface instanceof HTMLCanvasElement) || !region || surface.hidden)
		throw new Error('The exact picture and underlying paused video must both be present.');
	return new Promise<{
		picture: number;
		mediaTime: number;
		requestedTime: number;
		presentedFrames: number;
		paused: boolean;
	}>((resolve, reject) => {
		const observer = new MutationObserver(() => {
			const visibleSurface = region.querySelector('canvas[aria-label="Selected recorded picture"]');
			const video = region.querySelector('video');
			if (!(visibleSurface instanceof HTMLCanvasElement) || !visibleSurface.hidden || !video)
				return;
			observer.disconnect();
			clearTimeout(timer);
			const canvas = document.createElement('canvas');
			canvas.width = 64;
			canvas.height = 32;
			const ctx = canvas.getContext('2d')!;
			ctx.drawImage(video, 0, 0, 64, 32);
			let picture = 0;
			for (let bit = 0; bit < 4; bit++) {
				if (ctx.getImageData(8 + 16 * bit, 16, 1, 1).data[0] > 128) picture |= 1 << bit;
			}
			resolve({
				picture,
				mediaTime: Number(video.dataset.lastMediaTime),
				requestedTime: video.currentTime,
				presentedFrames: Number(video.dataset.presentedPictures),
				paused: video.paused
			});
		});
		const timer = setTimeout(() => {
			observer.disconnect();
			reject(
				new Error(
					'No verified resume picture: ' +
						region.querySelector('video')?.dataset.presentationHistory
				)
			);
		}, 8_000);
		observer.observe(region, {
			subtree: true,
			childList: true,
			attributes: true,
			attributeFilter: ['hidden']
		});
	});
}

const schedules = {
	cfr: [0, 40, 80, 120, 160, 200, 240, 280, 320, 360, 400, 440],
	vfr: [0, 40, 80, 200, 240, 280, 400, 440, 480, 600, 640, 680],
	'duplicate-pts': [0, 40, 40, 200, 240, 280, 400, 440, 480, 600, 640, 640]
} as const;

function timestampMs(time: string): number {
	const fields = /^(.*)\.(\d{9})Z$/.exec(time);
	if (!fields) throw new Error('The visible picture needs a nanosecond timestamp');
	return Date.parse(fields[1] + 'Z') + Number(fields[2]) / 1e6;
}

for (const fixture of ['cfr', 'vfr', 'duplicate-pts'] as const) {
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
			await page.addInitScript(observePlaybackPresentation);
			await page.goto(
				`/keep?camera=${encodeURIComponent(source)}&stream=sub&at=${start}&date=${new Date(start).toISOString().slice(0, 10)}`
			);
			const region = page.getByRole('region', { name: 'Recorded video player' });
			const video = region.locator('video');
			await expect
				.poll(() => video.evaluate((v) => (v instanceof HTMLVideoElement ? v.videoWidth : 0)), {
					timeout: 45_000
				})
				.toBe(64);
			await expect
				.poll(async () => Number(await video.getAttribute('data-presented-pictures')), {
					timeout: 15_000
				})
				.toBeGreaterThan(0);
			const pause = page.getByRole('button', { name: 'Pause recording', exact: true });
			if (!(await video.evaluate((v) => (v as HTMLVideoElement).paused))) await pause.click();
			const next = page.getByRole('button', { name: 'Next recorded frame', exact: true });
			const previous = page.getByRole('button', { name: 'Previous recorded frame', exact: true });
			const selected = region.locator('canvas[aria-label="Selected recorded picture"]');
			try {
				await expect(next).toBeEnabled();
			} catch (cause) {
				throw new Error(
					'Paused anchor: ' +
						JSON.stringify(
							await video.evaluate((v) => {
								const element = v as HTMLVideoElement;
								return {
									paused: element.paused,
									seeking: element.seeking,
									currentTime: element.currentTime,
									trace: element.dataset.presentationHistory
								};
							})
						),
					{ cause }
				);
			}
			const measurements = [];
			const bounds = { start: 0, end: 0 };
			let lastTime: number | undefined;
			let lastId = -1;
			const timeline = page.locator('[data-recording-timeline-playhead-ms]:visible').first();
			const clock = region.locator('time[data-frame-recording-time]');
			for (const [button, key] of [
				[next, 'ArrowRight'],
				[previous, 'ArrowLeft']
			] as const) {
				await expect(button).toHaveAttribute('aria-keyshortcuts', key);
				const box = await button.boundingBox();
				expect(box?.width).toBeGreaterThanOrEqual(44);
				expect(box?.height).toBeGreaterThanOrEqual(44);
			}
			await region.getByRole('button', { name: 'Digital zoom in', exact: true }).click();
			const zoom = region.getByRole('meter', { name: 'Digital zoom level' });
			await expect
				.poll(async () => Number(await zoom.getAttribute('aria-valuenow')))
				.toBeGreaterThan(1);
			const level = await zoom.getAttribute('aria-valuenow');
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
				const absoluteMs = timestampMs((await clock.getAttribute('datetime'))!);
				const sourceClock = region.locator('time[data-frame-source-time]');
				const sourceMs = (await sourceClock.count())
					? timestampMs((await sourceClock.getAttribute('datetime'))!)
					: absoluteMs;
				const recorded = Number(await region.getAttribute('data-recording-playhead-ms'));
				expect(Math.abs(recorded - absoluteMs)).toBeLessThan(0.001);
				await expect
					.poll(async () =>
						Number(await timeline.getAttribute('data-recording-timeline-playhead-ms'))
					)
					.toBe(recorded);
				if (lastTime !== undefined && Math.abs(lastId - id) === 1) {
					const delta = schedules[fixture][id] - schedules[fixture][lastId];
					expect(Math.abs(sourceMs - lastTime - delta)).toBeLessThan(0.03);
				}
				if (direction === 1 && measurements.length === 1) {
					await button.press('[');
					bounds.start = recorded;
				}
				if (direction === 1 && measurements.length === 10) {
					await button.press(']');
					bounds.end = recorded;
				}
				measurements.push({ direction, picture: id, elapsedMs: presented.elapsedMs, absoluteMs });
				lastTime = sourceMs;
				lastId = id;
			}
			await expect(zoom).toHaveAttribute('aria-valuenow', level!);
			await next.focus();
			const extra = (id + 1) % 12;
			await Promise.all([selected.evaluate(observePicture, extra), next.press('ArrowRight')]);
			await expect(next).toBeFocused();

			// Observe the visible canvas-to-video transition, while normal playback is still paused.
			const handoff = selected.evaluate(witnessHandoff);
			await region.getByRole('button', { name: 'Play recording', exact: true }).click();
			const resumed = await handoff;
			expect(resumed.picture, JSON.stringify(resumed)).toBe(extra);
			expect(resumed.paused).toBe(true);
			await expect(selected).toBeHidden();
			await expect(video).toHaveJSProperty('paused', false);
			const reorderedHandoffs = [];
			if (fixture === 'duplicate-pts') {
				for (const target of [3, 5, 6, 9, 11, 0]) {
					await pause.click();
					await expect(video).toHaveJSProperty('paused', true);
					let current = (await video.evaluate(observePicture, null)).id;
					// Every handoff starts with a real selected canvas, even when Pause landed on the target.
					const count = (target - current + 12) % 12 || 12;
					for (let step = 0; step < count; step++) {
						current = (current + 1) % 12;
						await Promise.all([selected.evaluate(observePicture, current), next.click()]);
					}
					const verified = selected.evaluate(witnessHandoff);
					await region.getByRole('button', { name: 'Play recording', exact: true }).click();
					const observed = await verified;
					expect(observed.picture, JSON.stringify(observed)).toBe(target);
					expect(observed.paused).toBe(true);
					await expect(video).toHaveJSProperty('paused', false);
					reorderedHandoffs.push(observed);
				}
			}
			await page.getByRole('button', { name: 'Export', exact: true }).click();
			const panel = page.locator('[data-keep-export]');
			await expect(panel).toHaveAttribute('data-export-start-ms', String(bounds.start));
			await expect(panel).toHaveAttribute('data-export-end-ms', String(bounds.end));
			for (const [label, value] of [
				['FROM', bounds.start],
				['TO', bounds.end]
			] as const) {
				const actual = await panel.getByLabel(label, { exact: true }).inputValue();
				expect(Date.parse('1970-01-01T' + actual + 'Z')).toBe(Math.floor(value) % 86_400_000);
			}
			let coldResume;
			if (fixture === 'cfr') {
				// Open on a later GOP, then step behind the cursor's first buffered picture.
				const later = start + 2_200;
				await page.goto(
					`/keep?camera=${encodeURIComponent(source)}&stream=sub&at=${later}&date=${new Date(later).toISOString().slice(0, 10)}`
				);
				await expect
					.poll(async () => Number(await video.getAttribute('data-presented-pictures')), {
						timeout: 20_000
					})
					.toBeGreaterThan(0);
				if (!(await video.evaluate((v) => (v as HTMLVideoElement).paused))) await pause.click();
				await expect(previous).toBeEnabled();
				const before = await region.evaluate((r) => {
					const element = r.querySelector('video')!;
					return {
						url: element.currentSrc,
						origin:
							Number(r.getAttribute('data-recording-playhead-ms')) - element.currentTime * 1_000
					};
				});
				let coldId = (await video.evaluate(observePicture, null)).id;
				let coldTime = Infinity;
				for (let i = 0; i < 24 && coldTime >= before.origin - 1; i++) {
					coldId = (coldId + 11) % 12;
					await Promise.all([selected.evaluate(observePicture, coldId), previous.click()]);
					coldTime = timestampMs((await clock.getAttribute('datetime'))!);
				}
				expect(coldTime).toBeLessThan(before.origin - 1);
				const decoded = selected.evaluate(witnessHandoff);
				await region.getByRole('button', { name: 'Play recording', exact: true }).click();
				coldResume = await decoded;
				expect(coldResume.picture, JSON.stringify(coldResume)).toBe(coldId);
				expect(coldResume.paused).toBe(true);
				await expect
					.poll(() => video.evaluate((v) => (v as HTMLVideoElement).currentSrc))
					.not.toBe(before.url);
				await expect(video).toHaveJSProperty('paused', false);
			}
			await testInfo.attach('input-to-recorded-picture', {
				body: JSON.stringify({
					fixture,
					measurements,
					bounds,
					resumed,
					reorderedHandoffs,
					coldResume
				}),
				contentType: 'application/json'
			});
		} finally {
			await recorder.close();
		}
	});
}
