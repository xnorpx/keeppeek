import { expect, type Locator, type Page, type Route } from '@playwright/test';

type WitnessVideo = HTMLVideoElement & { stopRecordedAudioWitness?: () => void };

export function recordedAudioBackendRoute(origin: string, backend: string) {
	return async (route: Route) => {
		const url = new URL(route.request().url());
		const applicationRequest =
			url.pathname === '/create' ||
			['/api/', '/auth/', '/config/'].some((prefix) => url.pathname.startsWith(prefix));
		if (url.origin === origin && applicationRequest) {
			await route.fulfill({
				response: await route.fetch({ url: `${backend}${url.pathname}${url.search}` })
			});
		} else await route.continue();
	};
}

async function decodedSnapshot(video: Locator) {
	return video.evaluate((element) => {
		if (!(element instanceof HTMLVideoElement)) throw new Error('Expected the recorded video.');
		const audio = element as HTMLVideoElement & { webkitAudioDecodedByteCount?: number };
		return {
			source: element.currentSrc,
			width: element.videoWidth,
			height: element.videoHeight,
			frames: element.getVideoPlaybackQuality().totalVideoFrames,
			audioDecodedBytes: audio.webkitAudioDecodedByteCount ?? 0,
			currentTime: element.currentTime,
			paused: element.paused,
			ended: element.ended,
			readyState: element.readyState,
			presentedTime: Number(element.dataset.recordedAudioPresentedTime ?? '-1'),
			presentedSource: element.dataset.recordedAudioPresentedSource ?? '',
			error: element.error?.message ?? null
		};
	});
}

async function witnessFrames(video: Locator, minimumTimeSeconds: number) {
	await video.evaluate((element, minimumTime) => {
		const media = element as WitnessVideo;
		media.stopRecordedAudioWitness?.();
		let callbackId: number | null = null;
		let callbacks = 0;
		let source = '';
		const presented: VideoFrameRequestCallback = (_, metadata) => {
			media.dataset.recordedAudioPresentedTime = String(metadata.mediaTime);
			media.dataset.recordedAudioPresentedSource = source;
			callbacks += 1;
			if (metadata.mediaTime < minimumTime && callbacks < 250) {
				callbackId = media.requestVideoFrameCallback(presented);
			}
		};
		const start = () => {
			if (callbackId !== null) media.cancelVideoFrameCallback(callbackId);
			source = media.currentSrc;
			media.dataset.recordedAudioPresentedTime = '-1';
			if (callbacks < 250) callbackId = media.requestVideoFrameCallback(presented);
		};
		media.addEventListener('loadeddata', start);
		media.stopRecordedAudioWitness = () => {
			media.removeEventListener('loadeddata', start);
			if (callbackId !== null) media.cancelVideoFrameCallback(callbackId);
		};
		start();
	}, minimumTimeSeconds);
}

export async function decodedRecordedAudio(
	page: Page,
	previousSource = '',
	minimumFrames = 0,
	minimumTimeSeconds = 0
) {
	const player = page.getByRole('region', { name: 'Recorded video player', exact: true });
	const video = player.locator('video');
	await expect(video).toBeVisible();
	await witnessFrames(video, minimumTimeSeconds);
	const play = player.getByRole('button', { name: 'Play recording', exact: true });
	if (await play.count()) await play.click();
	try {
		await expect
			.poll(
				async () => {
					const sample = await decodedSnapshot(video);
					return (
						sample.source !== previousSource &&
						sample.width === 160 &&
						sample.height === 90 &&
						sample.frames > minimumFrames &&
						sample.currentTime >= minimumTimeSeconds &&
						sample.presentedTime >= minimumTimeSeconds &&
						sample.presentedSource === sample.source &&
						sample.audioDecodedBytes > 0 &&
						sample.error === null
					);
				},
				{ timeout: 20_000 }
			)
			.toBe(true);
	} catch (cause) {
		throw new Error(
			`Recorded audio decode predicate failed: ${JSON.stringify({
				previousSource,
				minimumFrames,
				minimumTimeSeconds,
				sample: await decodedSnapshot(video)
			})}`,
			{ cause }
		);
	} finally {
		await video.evaluate((element) => (element as WitnessVideo).stopRecordedAudioWitness?.());
	}
	const sample = await decodedSnapshot(video);
	const pause = player.getByRole('button', { name: 'Pause recording', exact: true });
	if (await pause.count()) await pause.click();
	await expect(player.getByRole('alert')).toHaveCount(0);
	return sample;
}

export async function previewRecordedAudio(
	page: Page,
	offsetMs: number | null,
	previousSource: string
) {
	await page.getByRole('button', { name: /^Recorded audio preview/ }).click();
	const dialog = page.getByRole('dialog', { name: 'Recorded audio preview', exact: true });
	if (offsetMs === null) {
		await dialog.getByRole('button', { name: 'Reset preview', exact: true }).click();
	} else {
		await dialog
			.getByRole('spinbutton', { name: 'Preview offset (ms)', exact: true })
			.fill(String(offsetMs));
		await dialog.getByRole('button', { name: 'Apply preview', exact: true }).click();
	}
	const label =
		offsetMs === null
			? '+200 ms / configured'
			: `${offsetMs > 0 ? '+' : ''}${offsetMs} ms / preview`;
	await expect(dialog.locator('[data-recorded-audio-evidence]')).toContainText(label);
	await expect(dialog.locator('[data-recorded-audio-evidence]')).toContainText(
		'Audio present in the inspected source fragment.'
	);
	await dialog.getByRole('button', { name: 'Close', exact: true }).click();
	return decodedRecordedAudio(page, previousSource);
}

export async function inspectRecordedAudio(page: Page) {
	await page.getByRole('button', { name: /^Recorded audio preview/ }).click();
	const dialog = page.getByRole('dialog', { name: 'Recorded audio preview', exact: true });
	await expect(dialog.locator('[data-recorded-audio-evidence]')).toContainText(
		'+200 ms / configured'
	);
	await dialog.getByText('Normalized container timing', { exact: true }).click();
	await expect(dialog.locator('details')).not.toContainText('Unavailable');
	const text = await dialog.locator('[data-recorded-audio-evidence]').innerText();
	await dialog.getByRole('button', { name: 'Close', exact: true }).click();
	return text;
}
