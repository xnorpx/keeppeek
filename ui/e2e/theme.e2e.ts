import { expect, test } from '@playwright/test';
import { mockMixedHealth } from './fixtures/peek';

test('switches and persists one theme across Peek and Keep', async ({ page }) => {
	await mockMixedHealth(page);

	await page.goto('/');

	await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
	await expect(page.locator('html')).toHaveClass(/dark/);
	const lightToggle = page.getByRole('button', { name: 'Switch to light theme' });
	await expect(lightToggle).toBeVisible();
	await lightToggle.click();

	await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
	await expect(page.locator('html')).not.toHaveClass(/dark/);
	await expect(page.getByRole('button', { name: 'Switch to dark theme' })).toBeVisible();
	await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(239, 232, 218)');
	await expect(page.locator('[data-shell-rail]')).toHaveCSS(
		'background-color',
		'rgb(239, 232, 218)'
	);
	await expect(page.locator('[data-peek-camera="front-door"]')).toHaveCSS(
		'background-color',
		'rgb(10, 11, 12)'
	);
	await expect(page.locator('[data-peek-camera="back-yard"]')).toHaveCSS(
		'background-color',
		'rgb(248, 244, 236)'
	);
	await expect(
		page.locator('[data-peek-camera="back-yard"] [data-peek-camera-region="evidence"]')
	).toHaveCSS('color', 'rgb(28, 26, 22)');
	await expect(page.locator('[data-peek-camera="porch"]')).toHaveCSS(
		'border-color',
		'rgb(168, 115, 16)'
	);
	await expect
		.poll(() => page.evaluate(() => localStorage.getItem('keeppeek-theme')))
		.toBe('light');

	await page.getByRole('link', { name: 'Keep', exact: true }).click();
	await expect(page).toHaveTitle('Keep - KeepPeek');
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');

	await page.reload();
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
	await page.getByRole('button', { name: 'Switch to dark theme' }).click();
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
	await expect(page.locator('html')).toHaveClass(/dark/);
	await expect.poll(() => page.evaluate(() => localStorage.getItem('keeppeek-theme'))).toBe('dark');
});

test('applies persisted light roles before hydration without lightening video', async ({
	page
}) => {
	await page.addInitScript(() => localStorage.setItem('keeppeek-theme', 'light'));
	await mockMixedHealth(page);
	await page.goto('/', { waitUntil: 'domcontentloaded' });

	await expect(page.locator('html')).toHaveAttribute('data-theme-preference', 'light');
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
	await expect(page.locator('html')).not.toHaveClass(/dark/);
	await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(239, 232, 218)');
	await expect(page.locator('[data-peek-camera="front-door"]')).toHaveCSS(
		'background-color',
		'rgb(10, 11, 12)'
	);
	await expect(page.locator('[data-peek-camera="back-yard"]')).toHaveCSS(
		'background-color',
		'rgb(248, 244, 236)'
	);
});
