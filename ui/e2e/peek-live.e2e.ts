import { expect, test } from '@playwright/test';

const streams = [{ cameraId: '127.0.0.1', width: 640, height: 360 }] as const;
const skipsRealWebRtcOnWindowsCi = process.platform === 'win32' && Boolean(process.env.CI);

test('Peek presents native WebRTC frames without a canvas fallback', async ({ page }) => {
	test.skip(
		skipsRealWebRtcOnWindowsCi,
		'Windows CI does not establish or decode this real WebRTC H.264 stream.'
	);
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	const createResponse = page
		.waitForResponse(
			(response) => response.url().endsWith('/create') && response.request().method() === 'POST',
			{ timeout: 10_000 }
		)
		.catch(() => null);
	await page.goto('/');
	const response = await createResponse;
	if (response === null) {
		throw new Error(browserErrors.join('\n'));
	}
	expect(response.status(), await response.text()).toBe(201);

	for (const stream of streams) {
		const liveView = page.locator(`[data-camera-id="${stream.cameraId}"]`);
		const tile = page.locator(`[data-peek-camera="${stream.cameraId}"]`);
		await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
		await expect(liveView.locator('canvas')).toHaveCount(0);
		const video = liveView.locator('video');
		await expect(video).toBeVisible();
		await expect
			.poll(
				async () =>
					video.evaluate((element) => {
						const videoElement = element as HTMLVideoElement;
						const frames = videoElement.getVideoPlaybackQuality().totalVideoFrames;
						return `video:${videoElement.videoWidth}x${videoElement.videoHeight}:${frames}`;
					}),
				{ timeout: 30_000 }
			)
			.toMatch(new RegExp(`^video:${stream.width}x${stream.height}:[1-9]\\d*$`));
		await expect(liveView).toHaveAttribute('data-frame-activity', 'active');
		await expect(tile).toHaveAttribute('data-peek-camera-state', /^(?:live|degraded)$/);
		await expect(tile).not.toContainText('Reconnecting');
		await expect(tile).not.toContainText('NO SIGNAL');
	}
	expect(browserErrors).toEqual([]);
});

test('Peek diagnostics stays open without interrupting live playback', async ({ page }) => {
	test.skip(
		skipsRealWebRtcOnWindowsCi,
		'Windows CI does not establish the real WebRTC control channel used by this full-stack test.'
	);
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
