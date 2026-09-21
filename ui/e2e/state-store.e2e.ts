import { expect, test } from '@playwright/test';

test('real WebRTC clients converge and replace stale state after reconnect', async ({ page }) => {
	test.setTimeout(60_000);
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.goto('/');
	await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toBeVisible();
	const result = await page.evaluate(async () => {
		const modulePath = '/e2e/fixtures/state-store-browser.ts';
		const fixture = (await import(modulePath)) as typeof import('./fixtures/state-store-browser');
		return fixture.convergenceAndRecovery();
	});
	expect(result.initialRevision).toBe('1');
	expect(result.sequence).toBe('20');
	expect(result.recoveredKeys).toEqual(['watched/replacement']);
	expect(result.recoveredRevision).toBe(result.expectedRevision);
	expect(result.status).toBe('closed');
	expect(result.watchCount).toBe(0);
	expect(errors).toEqual([]);
});

test('a stalled WebRTC subscriber receives the acknowledgement timeout closure', async ({
	page
}) => {
	test.setTimeout(60_000);
	await page.goto('/');
	await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toBeVisible();
	const result = await page.evaluate(async () => {
		const modulePath = '/e2e/fixtures/state-store-browser.ts';
		const fixture = (await import(modulePath)) as typeof import('./fixtures/state-store-browser');
		return fixture.stalledWatchCloses();
	});
	expect(result.closeReason).toBe(result.expected);
});
