import { expect, test } from '@playwright/test';
import { keepModeDate, mockKeepModes } from './fixtures/keep-modes';

test('compares at most eight cameras on one shared recording clock', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	await mockKeepModes(page, 10);
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=swimlanes`);

	await expect(page.getByRole('button', { name: 'Swimlanes', exact: true })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await expect(page.locator('[data-swimlane]')).toHaveCount(8);
	await expect(page.locator('video')).toHaveCount(0);
	await expect(page.locator('[data-swimlane="camera-9"]')).toHaveCount(0);
	await page.locator('summary').filter({ hasText: 'Choose cameras' }).click();
	await expect(page.getByRole('button', { name: 'Camera 9', exact: true })).toBeDisabled();
	await expect(page.locator('[data-swimlane-gap]').first()).toBeVisible();

	await page.getByRole('button', { name: 'Camera 8', exact: true }).click();
	await page.getByRole('button', { name: 'Camera 9', exact: true }).click();
	await expect(page.locator('[data-swimlane="camera-9"]')).toBeVisible();
	await expect(page.locator('[data-swimlane]')).toHaveCount(8);

	await page.getByRole('button', { name: /Camera 2 person event/i }).click();
	await expect(page).toHaveURL(/camera=camera-2/);
	await expect(page).not.toHaveURL(/mode=swimlanes/);
	await expect(page.getByRole('button', { name: 'Timeline', exact: true })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await expect(page.locator('video[controls]')).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
