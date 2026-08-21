import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import { mockMixedHealth } from './fixtures/peek';

test('renders the KeepPeek dashboard without configured cameras', async ({ page }) => {
	await mockControlPeer(page, { health: { status: 'healthy', cameras: [] } });

	await page.goto('/');

	await expect(page).toHaveTitle('Peek - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Peek', exact: true })).toBeVisible();
	await expect(page.getByText('System online', { exact: true })).toBeVisible();
	await expect(page.getByText('0 cameras', { exact: true })).toBeVisible();
	await expect(page.getByText('No cameras configured.')).toBeVisible();
});

test('Board 6 renders live, degraded, reconnecting, and offline Paper tile states', async ({
	page
}) => {
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	await mockMixedHealth(page);
	await page.goto('/');

	await expect(page.locator('[data-peek-camera="front-door"]')).toHaveAttribute(
		'data-peek-camera-state',
		'live'
	);
	await expect(page.locator('[data-peek-camera="porch"]')).toHaveAttribute(
		'data-peek-camera-state',
		'degraded'
	);
	await expect(page.locator('[data-peek-camera="alley"]')).toHaveAttribute(
		'data-peek-camera-state',
		'reconnecting'
	);
	await expect(page.locator('[data-peek-camera="back-yard"]')).toHaveAttribute(
		'data-peek-camera-state',
		'offline'
	);
	await expect(
		page.locator('[data-peek-camera="porch"] [data-peek-camera-region="evidence"]')
	).toContainText('Degraded — 14% frames dropped');
	await expect(page.getByText('Attempt 3')).toBeVisible();
	await expect(page.getByText('Authentication failed')).toBeVisible();
	await expect(page.getByRole('link', { name: 'Diagnose' })).toBeVisible();
	await expect(page.locator('[data-peek-camera="front-door"]')).toContainText('REC');
	await expect(page.locator('[data-peek-camera="back-yard"]')).not.toContainText('REC');
	await page.locator('[data-peek-camera="porch"]').hover();
	await expect(page.getByRole('button', { name: 'Rewind Porch' })).toBeVisible();
	await page.locator('[data-peek-camera="back-yard"]').hover();
	await expect(page.getByRole('button', { name: 'Rewind Back Yard' })).toBeVisible();
	expect(browserErrors).toEqual([]);
});

test('keeps mixed Peek states usable at the authored mobile viewport', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockMixedHealth(page);
	await page.goto('/');

	await expect(page.locator('[data-peek-camera]')).toHaveCount(4);
	await expect(page.locator('[data-peek-camera="back-yard"]')).toBeVisible();
	const mobileRewind = page.getByRole('button', { name: 'Rewind Front Door' });
	await mobileRewind.focus();
	await expect(mobileRewind).toBeVisible();
	await page.keyboard.press('ArrowDown');
	await expect(page.locator('[data-peek-camera="front-door"] [data-peek-rewind]')).toHaveAttribute(
		'data-peek-rewind-seconds',
		'5'
	);
	await page.keyboard.press('Escape');
	await expect(page.locator('[data-peek-rewind]')).toHaveCount(0);
	await expect
		.poll(() =>
			page.locator('[data-peek-camera="front-door"]').evaluate((element) => {
				const bounds = element.getBoundingClientRect();
				return [Math.round(bounds.width), Math.round(bounds.height)];
			})
		)
		.toEqual([358, 201]);
	await expect
		.poll(() =>
			page.locator('[data-peek-camera="porch"]').evaluate((element) => {
				const bounds = element.getBoundingClientRect();
				return [Math.round(bounds.width), Math.round(bounds.height)];
			})
		)
		.toEqual([174, 110]);
	for (const tile of await page.locator('[data-peek-camera]').all()) {
		const overlaps = await tile.evaluate((element) => {
			const regions = Array.from(
				element.querySelectorAll<HTMLElement>('[data-peek-camera-region]')
			).filter((region) => {
				const bounds = region.getBoundingClientRect();
				return bounds.width > 0 && bounds.height > 0;
			});
			return regions.flatMap((region, regionIndex) => {
				const bounds = region.getBoundingClientRect();
				return regions.slice(regionIndex + 1).flatMap((otherRegion) => {
					const otherBounds = otherRegion.getBoundingClientRect();
					const overlapsHorizontally =
						bounds.left < otherBounds.right && bounds.right > otherBounds.left;
					const overlapsVertically =
						bounds.top < otherBounds.bottom && bounds.bottom > otherBounds.top;
					return overlapsHorizontally && overlapsVertically
						? [`${region.dataset.peekCameraRegion}:${otherRegion.dataset.peekCameraRegion}`]
						: [];
				});
			});
		});
		expect(overlaps).toEqual([]);
	}
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('names the negotiated first-keyframe wait and degrades it after five seconds', async ({
	page
}) => {
	await mockControlPeer(page, {
		cameras: [
			{
				id: 'alley',
				ip: '192.0.2.40',
				name: 'Alley',
				manufacturer: 'ONVIF',
				model: null,
				firmware_version: null,
				is_reolink: false,
				capabilities: {
					ptz: false,
					audio: false,
					events: false,
					recording: true,
					analytics: false,
					imaging: false,
					two_way_audio: false
				},
				profiles: [
					{
						name: 'Sub',
						stream: 'sub',
						encoding: 'h264',
						resolution: '640x360',
						framerate: 15
					}
				]
			}
		],
		health: {
			status: 'healthy',
			cameras: [
				{
					id: 'alley',
					state: 'online',
					lifecycle: 'Connected',
					last_error: null,
					streams: []
				}
			]
		}
	});
	await page.goto('/');

	const state = page.locator('[data-first-frame-state]');
	await expect(state).toHaveAttribute('data-first-frame-state', 'waiting');
	await expect(state).toContainText('Negotiated · waiting for a keyframe');
	await expect(state).toHaveAttribute('data-first-frame-state', 'late', { timeout: 7_000 });
	await expect(state).toContainText('No keyframe after');
});
