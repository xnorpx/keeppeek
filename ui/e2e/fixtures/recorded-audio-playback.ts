import { expect, type Locator, type Page, type Route } from '@playwright/test';

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

export async function installRecordedAudioFrameProbe(page: Page) {
	await page.addInitScript(() => {
		const installed = Symbol.for('keeppeek.recordedAudioFrameProbe');
		if (Reflect.has(document, installed)) return;
		Object.defineProperty(document, installed, { value: true });
		const pending = new WeakMap<HTMLVideoElement, number>();
		document.addEventListener(
			'loadstart',
			(event) => {
				const video = event.target;
				if (!(video instanceof HTMLVideoElement)) return;
				const previous = pending.get(video);
				if (previous !== undefined) video.cancelVideoFrameCallback(previous);
				delete video.dataset.recordedAudioPresentedSource;
				delete video.dataset.recordedAudioPresentedTime;
				const source = video.currentSrc || video.src;
				if (!source) return;
				const presented: VideoFrameRequestCallback = (_, metadata) => {
					if (video.currentSrc !== source) return;
					video.dataset.recordedAudioPresentedTime = String(metadata.mediaTime);
					video.dataset.recordedAudioPresentedSource = source;
					pending.set(video, video.requestVideoFrameCallback(presented));
				};
				pending.set(video, video.requestVideoFrameCallback(presented));
			},
			true
		);
	});
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
