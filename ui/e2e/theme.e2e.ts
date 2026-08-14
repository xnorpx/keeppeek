import { expect, test } from '@playwright/test';

test('switches and persists one theme across Peek and Keep', async ({ page }) => {
	await page.route('http://127.0.0.1:4174/health', async (route) => {
		await route.fulfill({ json: { status: 'ok', cameras: [] } });
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({ json: [] });
	});

	await page.goto('/');

	await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
	await expect(page.locator('html')).toHaveClass(/dark/);
	const lightToggle = page.getByRole('button', { name: 'Switch to light theme' });
	await expect(lightToggle).toBeVisible();
	await lightToggle.click();

	await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
	await expect(page.locator('html')).not.toHaveClass(/dark/);
	await expect(page.getByRole('button', { name: 'Switch to dark theme' })).toBeVisible();
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
