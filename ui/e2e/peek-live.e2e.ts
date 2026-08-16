import { expect, test } from '@playwright/test';

const streams = [{ cameraId: '127.0.0.1', width: 640, height: 360 }] as const;

test('Peek decodes frames from configured fake cameras', async ({ page }) => {
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	const offerResponse = page
		.waitForResponse(
			(response) =>
				response.url().endsWith('/api/live/browser/offer') &&
				response.request().method() === 'POST',
			{ timeout: 10_000 }
		)
		.catch(() => null);
	await page.goto('/');
	const response = await offerResponse;
	if (response === null) {
		throw new Error(browserErrors.join('\n'));
	}
	expect(response.status(), await response.text()).toBe(200);

	for (const stream of streams) {
		const liveView = page.locator(`[data-camera-id="${stream.cameraId}"]`);
		await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
		const video = liveView.locator('video');
		await expect
			.poll(
				async () =>
					video.evaluate((element) => {
						const videoElement = element as HTMLVideoElement;
						const canvas = element.parentElement?.querySelector<HTMLCanvasElement>('canvas');
						if (canvas && getComputedStyle(canvas).display !== 'none') {
							return `canvas:${canvas.width}x${canvas.height}`;
						}
						const frames = videoElement.getVideoPlaybackQuality().totalVideoFrames;
						return `video:${videoElement.videoWidth}x${videoElement.videoHeight}:${frames}`;
					}),
				{ timeout: 30_000 }
			)
			.toMatch(
				new RegExp(
					`^(canvas:${stream.width}x${stream.height}|video:${stream.width}x${stream.height}:[1-9]\\d*)$`
				)
			);
	}
	expect(browserErrors).toEqual([]);
});

test('Peek diagnostics stays open without interrupting live playback', async ({ page }) => {
	await page.goto('/');

	const stream = streams[0];
	const liveView = page.locator(`[data-camera-id="${stream.cameraId}"]`);
	await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
	const sessionId = await liveView.getAttribute('data-session-id');
	expect(sessionId).not.toBeNull();

	const trigger = liveView.getByRole('button', { name: 'WebRTC stream diagnostics' });
	await trigger.click();
	const diagnostics = page.locator(`[data-web-rtc-diagnostics="${stream.cameraId}"]`);
	await expect(diagnostics).toBeVisible();
	const [triggerBox, diagnosticsBox] = await Promise.all([
		trigger.boundingBox(),
		diagnostics.boundingBox()
	]);
	expect(triggerBox).not.toBeNull();
	expect(diagnosticsBox).not.toBeNull();
	expect(diagnosticsBox!.x + diagnosticsBox!.width).toBeLessThanOrEqual(triggerBox!.x);
	await page.mouse.move(0, 0);
	await expect(diagnostics).toBeVisible();
	await expect(liveView).toHaveAttribute('data-status', 'live');
	await expect(liveView).toHaveAttribute('data-session-id', sessionId ?? '');
});
