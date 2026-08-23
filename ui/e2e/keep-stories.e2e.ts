import { expect, test } from '@playwright/test';
import { keepModeDate, keepModeOlderDate, mockKeepModes } from './fixtures/keep-modes';

test('Board 9 Stories renders only server-authored events and returns to Timeline', async ({
	page
}) => {
	await mockKeepModes(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=stories`);

	await expect(page.getByRole('button', { name: 'Stories', exact: true })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await expect(page.locator('[data-keep-story]')).toHaveCount(1);
	await expect(
		page.getByText('Summary and additional frames were not reported by this server.')
	).toBeVisible();
	await expect(page.getByText('Camera event source', { exact: true })).toBeVisible();

	await page.getByRole('button', { name: /review story at/i }).click();
	await expect(page.getByRole('button', { name: 'Timeline', exact: true })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await expect(page).not.toHaveURL(/mode=stories/);
	await expect(
		page.getByRole('region', { name: 'Recorded video player' }).locator('video').first()
	).toHaveAttribute('data-play-requested', 'true');
});

test('Board 9 Calendar enables only footage-backed days', async ({ page }) => {
	await mockKeepModes(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=stories`);

	await expect(page.locator(`[data-calendar-date="${keepModeDate}"]`)).toBeEnabled();
	await expect(page.locator('[data-calendar-date="2026-08-16"]')).toBeDisabled();

	await page.locator(`[data-calendar-date="${keepModeOlderDate}"]`).click();
	await expect(page).toHaveURL(new RegExp(`date=${keepModeOlderDate}.*mode=stories`));
	await expect(page.getByText('No story events reported.')).toBeVisible();
	await page.locator(`[data-calendar-date="${keepModeDate}"]`).click();
	await expect(page.locator('[data-keep-story="story-1"]')).toBeVisible();
});

test('contains every Keep mode at the authored mobile viewport', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockKeepModes(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=stories`);

	await expect(page.getByRole('region', { name: 'Stories' })).toBeVisible();
	await expect(page.getByRole('region', { name: 'Footage calendar' })).toBeVisible();
	await page.getByRole('button', { name: 'Swimlanes', exact: true }).click();
	await expect(page.locator('[data-swimlane]')).toHaveCount(8);
	await expect
		.poll(() =>
			page
				.locator('[data-swimlane-scroll]')
				.evaluate((element) => element.scrollWidth > element.clientWidth)
		)
		.toBe(true);
	await page.getByRole('button', { name: 'Export', exact: true }).click();
	await expect(page.locator('[data-keep-export]')).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
