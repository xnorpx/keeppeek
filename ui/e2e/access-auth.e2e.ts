import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const accessKey = '550e8400-e29b-41d4-a716-446655440000';

async function browserCredentialArtifacts(page: import('@playwright/test').Page): Promise<string> {
	return page.evaluate(() =>
		JSON.stringify({
			url: window.location.href,
			html: document.documentElement.outerHTML,
			localStorage: Object.fromEntries(
				Array.from({ length: window.localStorage.length }, (_, index) => {
					const key = window.localStorage.key(index) ?? '';
					return [key, window.localStorage.getItem(key)];
				})
			),
			sessionStorage: Object.fromEntries(
				Array.from({ length: window.sessionStorage.length }, (_, index) => {
					const key = window.sessionStorage.key(index) ?? '';
					return [key, window.sessionStorage.getItem(key)];
				})
			)
		})
	);
}

test('remote User signs in without persistent token artifacts and returns on revocation', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const consoleMessages: string[] = [];
	page.on('console', (message) => consoleMessages.push(message.text()));
	const controls = await mockControlPeer(page, {
		requiredAccessKey: accessKey,
		accessLocal: false,
		accessRole: 'user',
		capabilityIds: ['keeppeek.identity.v1']
	});

	await page.goto('/');
	await expect(page.getByRole('heading', { name: 'Remote sign-in' })).toBeVisible();
	await expect(page.getByLabel('Access key')).toHaveAttribute('type', 'password');
	await expect(page.locator('[data-shell-rail]')).toHaveCount(0);
	await page.getByLabel('Access key').fill(accessKey);
	await page.getByRole('button', { name: 'Sign in' }).click();

	await expect(page.locator('[data-shell-context]')).toBeHidden();
	await expect(page.locator('[data-shell-status]')).toHaveCount(0);
	await expect(page.locator('[data-shell-status-indicators]')).toBeVisible();
	await expect(
		page.locator('[data-shell-rail-actions]').getByRole('button', { name: 'Sign out' })
	).toBeVisible();
	await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Settings' })).toHaveCount(0);
	await expect(page.getByRole('link', { name: 'Cameras' })).toHaveCount(0);
	await expect(page.getByRole('link', { name: 'Health' })).toHaveCount(0);
	expect(controls.createAuthorizations).toEqual([null, `Bearer ${accessKey}`]);
	expect(await browserCredentialArtifacts(page)).not.toContain(accessKey);
	expect(consoleMessages.join('\n')).not.toContain(accessKey);

	await page.evaluate(() => {
		(window as unknown as Window & { closeKeepPeekControl(): void }).closeKeepPeekControl();
	});
	await expect(page.getByRole('heading', { name: 'Remote sign-in' })).toBeVisible();
	await expect(
		page.getByText('The remote session expired, was revoked, or disconnected.')
	).toBeVisible();
	expect(await browserCredentialArtifacts(page)).not.toContain(accessKey);
	expect(consoleMessages.join('\n')).not.toContain(accessKey);
});
