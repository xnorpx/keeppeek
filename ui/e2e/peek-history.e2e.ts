import { expect, test } from '@playwright/test';

const cameraId = '127.0.0.1';

test('opens Keep history from a focused Peek camera without a rewind gesture', async ({ page }) => {
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

	await page.goto('/');
	const tile = page.locator(`[data-peek-camera="${cameraId}"]`);
	const liveView = tile.locator(`[data-camera-id="${cameraId}"]`);
	await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
	await expect(tile.getByRole('button', { name: /^Rewind / })).toHaveCount(0);

	await tile.getByRole('button', { name: /^Focus .* live view$/ }).click();
	const focusSurface = page.locator('[data-peek-focus-history]');
	await expect(focusSurface).toBeVisible();
	await expect(focusSurface.getByRole('button', { name: /^Rewind / })).toHaveCount(0);
	await expect(focusSurface.locator('[data-peek-rewind]')).toHaveCount(0);

	const history = page.getByRole('link', { name: 'History', exact: true });
	await expect(history).toHaveAttribute(
		'href',
		new RegExp(`/keep\\?camera=${cameraId.replaceAll('.', '\\.')}&stream=main`)
	);
	await history.click();
	await expect(page).toHaveURL(
		new RegExp(`/keep\\?camera=${cameraId.replaceAll('.', '\\.')}&stream=main`)
	);
	await expect(page).not.toHaveURL(/from=peek/);
	await expect(page.getByRole('heading', { name: 'Keep', exact: true })).toBeVisible();
});
