import { expect, test, type Locator, type Page } from '@playwright/test';
import { resolve } from 'node:path';
import { writeFile } from 'node:fs/promises';
import {
	cardSourceIds,
	cardTestKeys,
	startHomeAssistantServer
} from './fixtures/home-assistant-server';
import {
	decodedRecordedAudio,
	inspectRecordedAudio,
	installRecordedAudioFrameProbe,
	previewRecordedAudio,
	recordedAudioBackendRoute
} from './fixtures/recorded-audio-playback';
import type { CalibrationRecording } from './fixtures/calibration-recording';
import { correctedMseAppends, installRecordedAudioMseProbe } from './fixtures/recorded-audio-mse';
import { installRecordedAudioControlProbe } from './fixtures/recorded-audio-control';

function recordingLink(at: number) {
	return `/keep?camera=${cardSourceIds[0]}&stream=sub&at=${at}&date=${new Date(at).toISOString().slice(0, 10)}`;
}

async function signIn(page: Page) {
	await expect(page.getByRole('heading', { name: 'Remote sign-in', exact: true })).toBeVisible();
	await page.getByLabel('Access key', { exact: true }).fill(cardTestKeys[0]);
	await page.getByRole('button', { name: 'Sign in', exact: true }).click();
}

async function exerciseCalibration(
	page: Page,
	manifest: CalibrationRecording,
	observations: unknown[]
) {
	await page.goto(recordingLink(manifest.start_ms + 100));
	await signIn(page);
	const configured = await decodedRecordedAudio(page, '', 0, 2.25);
	observations.push({
		phase: 'configured',
		...configured,
		timing: await inspectRecordedAudio(page),
		appends: await correctedMseAppends(page, configured.source, 2)
	});
	const advanced = await previewRecordedAudio(page, -200, configured.source);
	observations.push({
		phase: 'advanced-preview',
		...advanced,
		appends: await correctedMseAppends(page, advanced.source)
	});
	const delayed = await previewRecordedAudio(page, 450, advanced.source);
	observations.push({
		phase: 'delayed-preview',
		...delayed,
		appends: await correctedMseAppends(page, delayed.source)
	});
	const reset = await previewRecordedAudio(page, null, delayed.source);
	observations.push({
		phase: 'reset',
		...reset,
		appends: await correctedMseAppends(page, reset.source)
	});
	const position = page.getByRole('slider', { name: 'Recording position', exact: true });
	await position.press('Home');
	await position.press('ArrowRight');
	await expect(position).toHaveValue('1');
	const sought = await decodedRecordedAudio(page, reset.source, 0, 0.9);
	expect(sought.currentTime).toBeGreaterThanOrEqual(0.9);
	observations.push({ phase: 'seek', ...sought });
	await page.goto(recordingLink(manifest.periods[1]!.start_ms + 100));
	await signIn(page);
	observations.push({
		phase: 'second-period',
		...(await decodedRecordedAudio(page)),
		timing: await inspectRecordedAudio(page)
	});
}

test('decodes recorded AAC through signed previews, reset, seek, and a second container period', async ({
	page,
	context,
	baseURL
}, info) => {
	test.setTimeout(120_000);
	if (!baseURL) throw new Error('Recorded audio calibration requires the configured UI origin.');
	const fixture = await startHomeAssistantServer(new URL(baseURL).origin, {
		trustedLocal: false,
		calibrationFixture: {
			mediaPath: resolve(
				import.meta.dirname,
				'../../crates/test-camera/testdata/flash-tone-lead-200ms.mp4'
			)
		}
	});
	const manifest = fixture.calibration;
	if (!manifest) throw new Error('The calibrated recording manifest is missing.');
	const routeBackend = recordedAudioBackendRoute(new URL(baseURL).origin, fixture.url);
	await context.route('**/*', routeBackend);
	await installRecordedAudioFrameProbe(page);
	await installRecordedAudioMseProbe(page);
	const controlErrors = await installRecordedAudioControlProbe(page);
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	const observations: unknown[] = [];
	try {
		await exerciseCalibration(page, manifest, observations);
		await page.screenshot({ path: info.outputPath('recorded-audio-calibration.png') });
		expect(errors).toEqual([]);
	} finally {
		try {
			const evidence = info.outputPath('recorded-audio-observations.json');
			await writeFile(
				evidence,
				JSON.stringify(
					{
						manifest,
						observations,
						errors,
						controlErrors: controlErrors(),
						serverLog: fixture.logs(),
						scope:
							'Real WebRTC/MSE H264 and AAC decode across two initializations on one corrected SourceBuffer, signed preview/reset, decoded seek and fresh second-period cursor; no browser acoustic synchronization or wire-generation ordering measurement.'
					},
					null,
					2
				)
			);
			await info.attach('recorded-audio-observations.json', {
				path: evidence,
				contentType: 'application/json'
			});
		} finally {
			try {
				await context.unroute('**/*', routeBackend);
			} finally {
				await fixture.close();
			}
		}
	}
});

async function countFrameRegistrations(video: Locator) {
	await video.evaluate((element) => {
		const video = element as HTMLVideoElement;
		const request = video.requestVideoFrameCallback.bind(video);
		let registered = 0;
		video.requestVideoFrameCallback = (callback) => {
			video.dataset.initialRegistrations = String(++registered);
			return request((now, frame) => {
				Reflect.deleteProperty(video, 'requestVideoFrameCallback');
				callback(now, frame);
			});
		};
	});
}

async function checkPausedFrame(video: Locator, source: string, time: number) {
	await expect
		.poll(() =>
			video.evaluate((element) => {
				const video = element as HTMLVideoElement;
				return {
					source: video.dataset.recordedAudioPresentedSource,
					paused: video.paused,
					current: video.currentTime,
					presented: Number(video.dataset.recordedAudioPresentedTime)
				};
			})
		)
		.toMatchObject({
			source,
			paused: true,
			current: expect.closeTo(time, 1),
			presented: expect.closeTo(time, 1)
		});
}

async function seekAfterContinuousPlayback(video: Locator) {
	return await video.evaluate(async (element) => {
		const video = element as HTMLVideoElement;
		video.loop = true;
		await video.play();
		await new Promise<void>((resolve) => {
			let frames = 0;
			const count = () => {
				if (++frames === 260) resolve();
				else video.requestVideoFrameCallback(count);
			};
			video.requestVideoFrameCallback(count);
		});
		video.pause();
		// Deliberately seek away from the last painted frame on this same object URL.
		const target = video.currentTime > 2 ? 0.5 : 3;
		video.currentTime = target;
		return target;
	});
}

test('witnesses a paused replacement recording before later assertions attach', async ({
	page,
	baseURL
}) => {
	test.setTimeout(60_000);
	if (!baseURL) throw new Error('Recorded audio calibration requires the configured UI origin.');
	await installRecordedAudioFrameProbe(page);
	await installRecordedAudioFrameProbe(page);
	await page.route('**/recorded-frame-witness', (route) =>
		route.fulfill({
			contentType: 'text/html',
			body: '<video muted preload="auto" width="160" height="90"></video>'
		})
	);
	await page.route('**/witness.mp4', (route) =>
		route.fulfill({
			contentType: 'video/mp4',
			path: resolve(
				import.meta.dirname,
				'../../crates/test-camera/testdata/flash-tone-lead-200ms.mp4'
			)
		})
	);
	await page.goto(new URL('/recorded-frame-witness', baseURL).href);
	const video = page.locator('video');
	await countFrameRegistrations(video);
    const sources: string[] = [];
    try {
        for (const time of [0, 1]) {
            const source = await video.evaluate(async () => URL.createObjectURL(await (await fetch('/witness.mp4')).blob()));
            sources.push(source);
            await video.evaluate((element, { source, time }) => {
                const video = element as HTMLVideoElement;
                video.addEventListener('loadedmetadata', () => {video.currentTime = time;}, { once: true });
                video.src = source;
            }, {source, time});
            await checkPausedFrame(video, source, time);
            await expect(video).toHaveAttribute('data-initial-registrations', '1');
            if (time === 1) {
                const sought = await seekAfterContinuousPlayback(video);
                await checkPausedFrame(video, source, sought);
            }
        }
    } finally {
        await video.evaluate((element, sources) => {
            const video = element as HTMLVideoElement;
            video.removeAttribute('src');
            video.load();
            for (const source of sources) URL.revokeObjectURL(source);
        }, sources);
    }

});
