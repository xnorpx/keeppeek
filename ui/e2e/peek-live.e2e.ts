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
		await expect(tile).toHaveAttribute('data-peek-camera-state', /^(?:starting|reconnecting)$/);
		await expect(tile).not.toContainText('HEALTHY');
		await expect(tile).not.toContainText('NO SIGNAL');
	}
	const wall = page.locator('[data-peek-wall]');
	await expect(wall).toHaveAttribute('data-peek-wall-state', 'ready');
	await expect(wall).toHaveAttribute('data-peek-wall-reveal', 'frames');
	await expect(wall).toHaveAttribute('data-peek-wall-ready-count', String(streams.length));
	await expect(wall).toHaveAttribute('data-peek-wall-target-count', String(streams.length));
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

	const trigger = liveView.getByRole('button', { name: /camera information/ });
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

test('leaves and re-enters Peek without a failed session delete', async ({ page }) => {
	test.skip(
		skipsRealWebRtcOnWindowsCi,
		'Windows CI does not establish the real WebRTC control channel used by this full-stack test.'
	);
	const browserErrors: string[] = [];
	const requestFailures: string[] = [];
	page.on('console', (message) => {
		if (
			message.type() === 'error' ||
			(message.type() === 'warning' && message.text().includes('svelte'))
		) {
			browserErrors.push(message.text());
		}
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	page.on('requestfailed', (request) => {
		requestFailures.push(`${request.method()} ${new URL(request.url()).pathname}`);
	});
	await page.goto('/');
	const liveView = page.locator(`[data-camera-id="${streams[0].cameraId}"]`);
	await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });

	const deleted = page.waitForResponse(
		(response) => response.url().endsWith('/delete') && response.request().method() === 'POST'
	);
	await page.getByRole('link', { name: 'Keep', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Keep', exact: true })).toBeVisible();
	expect((await deleted).status()).toBe(200);
	await page.getByRole('link', { name: 'Settings', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible();
	await expect(page.getByText('Settings unavailable', { exact: true })).toHaveCount(0);
	await page.getByRole('link', { name: 'Peek', exact: true }).click();
	await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });

	expect(requestFailures).toEqual([]);
	expect(browserErrors).toEqual([]);
});

test('Peek focus automatic quality starts on the main stream and preserves explicit switches', async ({
	page
}) => {
	test.setTimeout(75_000);
	test.skip(
		skipsRealWebRtcOnWindowsCi,
		'Windows CI does not establish the real WebRTC control channel used by this full-stack test.'
	);
	const svelteWarnings: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'warning' && message.text().includes('[svelte]')) {
			svelteWarnings.push(message.text());
		}
	});
	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto('/');

	const tile = page.locator(`[data-peek-camera="${streams[0].cameraId}"]`);
	await tile.getByRole('button', { name: /^Focus .* live view$/ }).click();
	const focus = page.getByRole('region', { name: /focus$/ });
	const liveView = focus.locator(`[data-camera-id="${streams[0].cameraId}"]`);
	const video = liveView.locator('video');
	await expect(liveView).toHaveAttribute('data-requested-quality', 'auto');
	await expect(liveView).toHaveAttribute('data-stream', 'main');
	await expect(liveView).not.toHaveAttribute('data-pending-stream');
	await expect
		.poll(() =>
			video.evaluate((element) => {
				const media = element as HTMLVideoElement;
				return `${media.videoWidth}x${media.videoHeight}:${media.getVideoPlaybackQuality().totalVideoFrames}`;
			})
		)
		.toMatch(/^640x360:[1-9]\d*$/);
	await expect
		.poll(() =>
			liveView.evaluate((element) => Number.parseFloat((element as HTMLElement).style.aspectRatio))
		)
		.toBeCloseTo(640 / 360, 5);

	await focus.getByRole('button', { name: 'Low', exact: true }).click();
	await expect(liveView).toHaveAttribute('data-requested-quality', 'low');
	await expect(liveView).toHaveAttribute('data-stream', 'main');
	await expect(liveView).toHaveAttribute('data-pending-stream', 'sub');
	await expect(liveView).toHaveAttribute('data-status', 'live');
	await expect(focus.locator('[data-peek-quality-switch]')).toContainText(
		'Switching to low stream…'
	);
	await expect(focus.getByRole('button', { name: 'High', exact: true })).toBeEnabled();
	await expect(liveView).toHaveAttribute('data-stream', 'sub', { timeout: 20_000 });
	await expect(liveView).not.toHaveAttribute('data-pending-stream');
	await expect(focus.locator('[data-peek-quality-switch]')).toHaveCount(0);

	await focus.getByRole('button', { name: 'High', exact: true }).click();
	await expect(liveView).toHaveAttribute('data-requested-quality', 'high');
	await expect(liveView).toHaveAttribute('data-stream', 'main');
	await focus.getByRole('button', { name: 'Low', exact: true }).click();
	await expect(liveView).toHaveAttribute('data-requested-quality', 'low');
	await expect(liveView).toHaveAttribute('data-stream', 'main');
	await expect(liveView).toHaveAttribute('data-pending-stream', 'sub');
	await expect(focus.locator('[data-peek-quality-switch]')).toContainText(
		'Switching to low stream…'
	);
	await expect(focus.getByRole('button', { name: 'High', exact: true })).toBeEnabled();
	await expect(liveView).toHaveAttribute('data-stream', 'sub', { timeout: 20_000 });
	await expect(liveView).not.toHaveAttribute('data-pending-stream');
	await expect(focus.locator('[data-peek-quality-switch]')).toHaveCount(0);

	await focus.getByRole('button', { name: 'High', exact: true }).click();
	await expect(liveView).toHaveAttribute('data-stream', 'main', { timeout: 20_000 });
	await focus.getByRole('button', { name: 'Return to camera grid' }).click();
	const wall = page.locator('[data-peek-wall]');
	await expect(wall).toHaveAttribute('data-peek-wall-state', 'ready', { timeout: 10_000 });
	await expect(wall.locator(`[data-camera-id="${streams[0].cameraId}"]`)).toHaveAttribute(
		'data-status',
		'live'
	);
	expect(svelteWarnings).toEqual([]);
});
