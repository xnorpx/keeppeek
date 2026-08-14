import { expect, test } from '@playwright/test';

test('streams a real tracing event through the server into headless Chromium', async ({ page }) => {
	await page.goto('/settings/logs');

	await expect(page.getByRole('heading', { name: 'Logs' })).toBeVisible();
	await expect(page.getByText('connected', { exact: true })).toBeVisible();
	await expect(page.getByText('Starting KeepPeek - press Ctrl+C to stop')).toBeVisible();

	const filter = 'info,keeppeek=debug,logging_fullstack=trace';
	await page.getByLabel('Server capture filter').fill(filter);
	await page.getByRole('button', { name: 'Save filter' }).click();

	await expect(page.getByText(`Active: ${filter}`)).toBeVisible();
	await expect(page.getByText('reloaded log filter', { exact: true })).toBeVisible();
	await expect(page.getByText('keeppeek::logging', { exact: true })).toBeVisible();
});
