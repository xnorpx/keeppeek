import { test as base, expect, type Page } from '@playwright/test';
import { writeFile } from 'node:fs/promises';
import { createHomeAssistantContainer } from './container';

export const test = base.extend<{
	homeAssistant: Awaited<ReturnType<typeof createHomeAssistantContainer>>;
}>({
	homeAssistant: async ({ browserName }, use, testInfo) => {
		expect(browserName).toBe('chromium');
		const environment = await createHomeAssistantContainer();
		try {
			await environment.start();
			await use(environment);
		} finally {
			try {
				const logPath = testInfo.outputPath('sanitized-server.log');
				await writeFile(logPath, await environment.logs());
				await testInfo.attach('sanitized-server-logs', {
					path: logPath,
					contentType: 'text/plain'
				});
			} finally {
				await environment.close();
			}
		}
	}
});

export async function onboardHomeAssistant(page: Page, origin: string): Promise<void> {
	await page.context().grantPermissions(['local-network-access'], { origin });
	await page.goto(origin);
	await page.getByRole('button', { name: 'Create my smart home' }).click();
	await page.getByRole('textbox', { name: /^Name\*?$/ }).fill('KeepPeek Tester');
	await page.getByRole('textbox', { name: /^Username\*?$/ }).fill('keeppeek-tester');
	await page.getByRole('textbox', { name: /^Password\*?$/ }).fill('local-ha-test-password');
	await page.getByRole('textbox', { name: /^Confirm password\*?$/ }).fill('local-ha-test-password');
	await page.getByRole('button', { name: 'Create account', exact: true }).click();
	await page.getByRole('button', { name: 'Next', exact: true }).click();
	await expect(
		page.getByText(
			'We would like to know the country your home is in, so we can use the correct units.'
		)
	).toBeVisible();
	await page.getByRole('button', { name: 'Next', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Help us help you' })).toBeVisible();
	await page.getByRole('button', { name: 'Next', exact: true }).click();
	await page.getByRole('button', { name: 'Finish', exact: true }).click();
	await expect(page.locator('home-assistant')).toBeVisible();
}
