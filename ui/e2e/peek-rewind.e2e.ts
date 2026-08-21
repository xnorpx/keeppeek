import { expect, test } from '@playwright/test';

const cameraId = '127.0.0.1';

test('rewinds one Peek tile into Keep without renegotiating the live peer', async ({ page }) => {
	test.skip(
		process.platform === 'win32' && Boolean(process.env.CI),
		'Windows CI browsers do not expose decoded H.264 frames for this WebRTC stream.'
	);
	await page.setViewportSize({ width: 1440, height: 840 });
	await page.addInitScript(() => {
		Object.defineProperty(HTMLMediaElement.prototype, 'play', {
			configurable: true,
			value() {
				this.dataset.playRequested = 'true';
				return Promise.resolve();
			}
		});
	});
	let offerCount = 0;
	const directMediaRequests: string[] = [];
	page.on('request', (request) => {
		if (request.url().endsWith('/create') && request.method() === 'POST') {
			offerCount += 1;
		}
		const pathname = new URL(request.url()).pathname;
		if (
			(request.resourceType() === 'fetch' || request.resourceType() === 'xhr') &&
			(pathname.includes('recordings') || pathname.includes('events'))
		) {
			directMediaRequests.push(request.url());
		}
	});

	await page.goto('/');
	const tile = page.locator(`[data-peek-camera="${cameraId}"]`);
	const liveView = tile.locator(`[data-camera-id="${cameraId}"]`);
	await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
	const initialSessionId = await liveView.getAttribute('data-session-id');
	expect(initialSessionId).not.toBeNull();
	expect(offerCount).toBe(2);
	const initialOfferCount = offerCount;

	await tile.hover();
	const rewindHandle = tile.getByRole('button', { name: /^Rewind / });
	await expect(rewindHandle).toBeVisible();
	await rewindHandle.click();
	await expect(page).toHaveURL(/\/$/);

	await rewindHandle.focus();
	await page.keyboard.press('ArrowDown');
	await expect(tile.locator('[data-peek-rewind]')).toHaveAttribute('data-peek-rewind-seconds', '5');
	await page.keyboard.press('Escape');
	await expect(tile.locator('[data-peek-rewind]')).toHaveCount(0);

	const handleBounds = await rewindHandle.boundingBox();
	expect(handleBounds).not.toBeNull();
	if (handleBounds === null) throw new Error('Rewind handle geometry is unavailable');
	await page.mouse.move(
		handleBounds.x + handleBounds.width / 2,
		handleBounds.y + handleBounds.height / 2
	);
	await page.mouse.down();
	await page.mouse.move(
		handleBounds.x + handleBounds.width / 2,
		handleBounds.y + handleBounds.height / 2 + 80,
		{ steps: 4 }
	);
	const rewindOverlay = tile.locator('[data-peek-rewind]');
	await expect(rewindOverlay).toBeVisible();
	const rewindSeconds = Number(await rewindOverlay.getAttribute('data-peek-rewind-seconds'));
	expect(rewindSeconds).toBeGreaterThan(20);
	expect(rewindSeconds).toBeLessThanOrEqual(120);
	await page.mouse.up();

	await expect(page).toHaveURL(/\/keep\?.*from=peek/);
	const targetTimestampMs = Number(new URL(page.url()).searchParams.get('at'));
	expect(Number.isSafeInteger(targetTimestampMs)).toBe(true);
	const peekEntry = page.locator('[data-peek-entry]');
	await expect(peekEntry).toBeVisible();
	await expect(peekEntry).toHaveAttribute('data-peek-entry-timestamp', String(targetTimestampMs));
	await expect(page.locator('video[src^="blob:"]')).toBeVisible();
	expect(offerCount).toBe(initialOfferCount);
	const sessionCountAfterStoredOpen = offerCount;
	expect(directMediaRequests).toEqual([]);

	await peekEntry.click();
	await expect(page).toHaveURL(/\/$/);
	const restoredLiveView = page.locator(`[data-camera-id="${cameraId}"]`);
	await expect(restoredLiveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
	await expect(restoredLiveView).toHaveAttribute('data-session-id', initialSessionId ?? '');
	expect(offerCount).toBe(sessionCountAfterStoredOpen);

	const restoredRewindHandle = page
		.locator(`[data-peek-camera="${cameraId}"]`)
		.getByRole('button', { name: /^Rewind / });
	await restoredRewindHandle.focus();
	await page.keyboard.press('ArrowDown');
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(/\/keep\?.*from=peek/);
	expect(offerCount).toBe(sessionCountAfterStoredOpen);
	await page.reload();
	await expect(page).toHaveURL(/\/$/);
});
