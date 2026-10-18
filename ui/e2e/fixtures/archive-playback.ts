import { expect, type Locator, type Page } from '@playwright/test';
import { cardSourceIds } from './home-assistant-server';

export async function installArchiveVideoProbe(page: Page) {
	await page.addInitScript(() => {
		const seen = new WeakSet<HTMLVideoElement>();
		const attach = () => {
			for (const video of document.querySelectorAll('video')) {
				if (seen.has(video)) continue;
				seen.add(video);
				video.addEventListener('loadstart', () => {
					const loadedAt = performance.now();
					delete video.dataset.archiveFrameSource;
					video.requestVideoFrameCallback((presentedAt) => {
						video.dataset.archiveFrameSource = video.currentSrc;
						video.dataset.archivePresentedAt = String(presentedAt);
						video.dataset.archiveLoadToPresentedMs = String(presentedAt - loadedAt);
					});
				});
			}
		};
		new MutationObserver(attach).observe(document, { childList: true, subtree: true });
		attach();
	});
}

async function frame(video: Locator) {
	return video.evaluate((element) => {
		if (!(element instanceof HTMLVideoElement)) throw new Error('Expected the recorded video.');
		return {
			source: element.currentSrc,
			presentedSource: element.dataset.archiveFrameSource ?? '',
			presentedAt: Number(element.dataset.archivePresentedAt),
			loadToPresentedMs: Number(element.dataset.archiveLoadToPresentedMs),
			decoded: element.readyState >= 2 && element.videoWidth > 0
		};
	});
}

async function presentedFrame(video: Locator, previousSource = '') {
	await expect
		.poll(
			async () => {
				const sample = await frame(video);
				return (
					!!sample.source &&
					sample.source !== previousSource &&
					sample.presentedSource === sample.source &&
					sample.decoded
				);
			},
			{ timeout: 20_000 }
		)
		.toBe(true);
	return frame(video);
}

export async function measureArchivePlayback(
	page: Page,
	moment: number,
	storage: 'local' | 'archive-initial' | 'archive-repeat'
) {
	await page.goto(
		`/keep?camera=${encodeURIComponent(cardSourceIds[0])}&stream=sub&at=${moment}&date=${new Date(moment).toISOString().slice(0, 10)}`
	);
	const notice = page.getByRole('status', { name: 'Archive playback', exact: true });
	if (storage === 'local') await expect(notice).toHaveCount(0);
	else await expect(notice).toBeVisible();
	const video = page.getByRole('region', { name: 'Recorded video player' }).locator('video');
	const initial = await presentedFrame(video);
	const pause = page.getByRole('button', { name: 'Pause recording', exact: true });
	if (await pause.count()) await pause.click();
	await expect
		.poll(() => video.evaluate((element) => element instanceof HTMLVideoElement && element.paused))
		.toBe(true);
	const position = page.getByRole('slider', { name: 'Recording position', exact: true });
	const bounds = await position.boundingBox();
	if (!bounds) throw new Error('Recording position control is not visible.');
	const requestedAt = await page.evaluate(() => performance.now());
	await position.click({ position: { x: bounds.width * 0.75, y: bounds.height / 2 } });
	await expect.poll(async () => Number(await position.inputValue())).toBeGreaterThan(25);
	const seek = await presentedFrame(video, initial.source);
	if (storage !== 'local') await expect(notice).toBeVisible();
	await expect(
		page.getByRole('region', { name: 'Recorded video player' }).getByRole('alert')
	).toHaveCount(0);
	return {
		storage,
		navigationToFirstPresentedMs: initial.presentedAt,
		firstLoadToPresentedMs: initial.loadToPresentedMs,
		seekToPresentedMs: seek.presentedAt - requestedAt,
		seekLoadToPresentedMs: seek.loadToPresentedMs
	};
}

export async function liveFrames(page: Page) {
	return page
		.locator(`[data-camera-id="${cardSourceIds[1]}"] video`)
		.first()
		.evaluate((element) => {
			if (!(element instanceof HTMLVideoElement)) throw new Error('Expected the live video.');
			return element.getVideoPlaybackQuality().totalVideoFrames;
		});
}
