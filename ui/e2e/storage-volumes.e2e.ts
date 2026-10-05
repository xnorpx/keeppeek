import { expect, test } from '@playwright/test';

test('named defaults support probes and cancellable drafts on desktop and mobile', async ({
	page
}, info) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto('/settings#storage');
	const volumes = page.getByRole('region', { name: 'Named storage volumes', exact: true });
	await expect(volumes.getByText('Volume runtime is available.', { exact: true })).toBeVisible();
	for (const id of ['media', 'exports', 'images', 'metadata']) {
		await expect(volumes.getByText(`${id} · Online`, { exact: true })).toBeVisible();
	}
	await volumes.getByRole('button', { name: 'Probe media', exact: true }).click();
	await expect(volumes.getByRole('button', { name: 'Probe media', exact: true })).toBeEnabled();
	await expect(volumes.getByRole('alert')).toHaveCount(0);
	await volumes.getByRole('button', { name: 'Refresh move jobs', exact: true }).click();
	await expect(volumes.getByText('No move jobs on this page.', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Change storage', exact: true }).click();
	await expect(page.getByLabel('Folder path', { exact: true })).toBeDisabled();
	await page.getByText('Advanced storage paths and writer controls', { exact: true }).click();
	await expect(page.getByLabel('Recording catalog path', { exact: true })).toBeDisabled();
	await page
		.locator('#storage-settings-editor')
		.getByRole('button', { name: 'Cancel', exact: true })
		.click();
	await volumes.getByRole('button', { name: 'Edit volume draft', exact: true }).click();
	const draft = volumes.getByRole('form', { name: 'Named volume draft' });
	const media = draft.getByRole('group', { name: 'Volume 1', exact: true });
	await expect(media.getByLabel('State', { exact: true })).toHaveValue('1');
	await media
		.getByLabel('Capacity in bytes (blank means unlimited)', { exact: true })
		.fill('12345');
	await expect(draft.getByRole('button', { name: 'Save volume draft', exact: true })).toBeEnabled();
	await page.screenshot({ path: info.outputPath('named-volumes-desktop.png'), fullPage: true });
	await page.setViewportSize({ width: 390, height: 844 });
	await expect(draft).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
		.toBe(true);
	await page.screenshot({ path: info.outputPath('named-volumes-mobile.png'), fullPage: true });
	page.once('dialog', (dialog) => dialog.accept());
	await draft.getByRole('button', { name: 'Cancel draft', exact: true }).click();
	await expect(draft).toHaveCount(0);
	await volumes.getByRole('button', { name: 'Edit volume draft', exact: true }).click();
	await expect(
		media.getByLabel('Capacity in bytes (blank means unlimited)', { exact: true })
	).toHaveValue('');
	await draft.getByRole('button', { name: 'Cancel draft', exact: true }).click();
	expect(errors).toEqual([]);
});
