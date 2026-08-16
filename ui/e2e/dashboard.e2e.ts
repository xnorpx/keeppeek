import { expect, test } from '@playwright/test';

test('renders the KeepPeek dashboard without configured cameras', async ({ page }) => {
	await page.route('**/health', async (route) => {
		await route.fulfill({ json: { status: 'ok', cameras: [] } });
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({ json: [] });
	});

	await page.goto('/');

	await expect(page).toHaveTitle('Peek - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Peek', exact: true })).toBeVisible();
	await expect(page.getByText('System online', { exact: true })).toBeVisible();
	await expect(page.getByText('0 cameras', { exact: true })).toBeVisible();
	await expect(page.getByText('No cameras configured.')).toBeVisible();
});




