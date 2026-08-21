import { expect, test } from '@playwright/test';
import { mockCameraFleet } from './fixtures/camera-fleet';

test('Board 11 virtualizes the 127-source Camera fleet into fixed 56px rows', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	await mockCameraFleet(page);

	await page.goto('/cameras');

	await expect(page).toHaveTitle('Cameras - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Cameras', exact: true })).toBeVisible();
	await expect(page.getByText('127 OF 127 SOURCES', { exact: true })).toBeVisible();
	await expect(page.locator('video')).toHaveCount(0);
	await expect(page.locator('[data-fleet-row]')).toHaveCount(24);
	await expect(page.locator('[data-fleet-row="front-door"]')).toBeVisible();
	await expect(page.locator('[data-fleet-row="camera-025"]')).toHaveCount(0);
	await expect
		.poll(() =>
			page
				.locator('[data-fleet-row]')
				.evaluateAll((rows) =>
					rows.every((row) => Math.round(row.getBoundingClientRect().height) === 56)
				)
		)
		.toBe(true);

	const viewport = page.locator('[data-fleet-viewport]');
	await viewport.evaluate((element) => {
		element.scrollTop = 56 * 100;
		element.dispatchEvent(new Event('scroll'));
	});
	await expect(page.locator('[data-fleet-row="camera-101"]')).toBeVisible();
	await expect(page.locator('[data-fleet-row]')).toHaveCount(24);
	await viewport.evaluate((element) => {
		element.scrollTop = element.scrollHeight;
		element.dispatchEvent(new Event('scroll'));
	});
	await expect(page.locator('[data-fleet-row="camera-127"]')).toBeVisible();

	await page.getByRole('searchbox', { name: 'Search cameras' }).fill('Camera 127');
	await expect(viewport).toHaveAttribute('data-fleet-total', '1');
	await expect(page.locator('[data-fleet-row]')).toHaveCount(1);
	await expect(page.getByRole('link', { name: 'Camera 127', exact: true })).toBeVisible();
	await page.getByRole('searchbox', { name: 'Search cameras' }).fill('');
	await page.getByRole('button', { name: /Not healthy/ }).click();
	await expect(viewport).toHaveAttribute('data-fleet-total', '2');
	await expect(page.locator('[data-fleet-row]')).toHaveCount(2);
	await expect(page.getByText('DEGRADED · 14% frames dropped')).toBeVisible();
	await expect(page.getByText('OFFLINE · Authentication failed')).toBeVisible();

	await page.getByRole('checkbox', { name: 'Select Porch' }).check();
	await expect(page.getByText('1 selected', { exact: true })).toBeVisible();
	await expect(page.getByText('Manage selected', { exact: true })).toHaveCount(0);
	await expect(
		page.locator('[data-capability-gate][data-capability="keeppeek.runtime-config.v1"]')
	).toHaveCount(0);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	expect(browserErrors).toEqual([]);
});

test('contains the virtualized fleet inside the authored mobile viewport', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockCameraFleet(page);

	await page.goto('/cameras');

	await expect(page.locator('[data-fleet-row]')).toHaveCount(24);
	await expect(page.getByRole('searchbox', { name: 'Search cameras' })).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	await expect
		.poll(() =>
			page
				.locator('[data-fleet-table-scroll]')
				.evaluate((element) => element.scrollWidth > element.clientWidth)
		)
		.toBe(true);
});

test('preserves fleet table lanes and row height while health evidence is pending', async ({
	page
}) => {
	let releaseHealth!: () => void;
	const healthGate = new Promise<void>((resolve) => {
		releaseHealth = resolve;
	});
	await mockCameraFleet(page, 42, { healthGate });
	await page.goto('/cameras');

	const skeleton = page.locator('[data-fleet-skeleton]');
	await expect(skeleton).toBeVisible();
	await expect(skeleton).toContainText('Reading health evidence · 42 cameras in inventory');
	await expect(skeleton.getByText('CAMERA', { exact: true })).toBeVisible();
	await expect(skeleton.getByText('TRANSPORT', { exact: true })).toBeVisible();
	await expect(skeleton.getByText('RECORDING', { exact: true })).toBeVisible();
	await expect
		.poll(() =>
			skeleton
				.locator('.h-14')
				.evaluateAll((rows) =>
					rows.every((row) => Math.round(row.getBoundingClientRect().height) === 56)
				)
		)
		.toBe(true);
	await expect(skeleton.locator('[data-slot="skeleton"]')).toHaveCount(0);

	releaseHealth();
	await expect(skeleton).toHaveCount(0);
	await expect(page.locator('[data-fleet-row]')).toHaveCount(24);
});
