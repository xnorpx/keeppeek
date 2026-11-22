import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { expect, test, type Locator, type Page } from '@playwright/test';
import { startAuthenticationFixture } from './fixtures/external-authentication-server';

async function downloadExport(page: Page, link: Locator) {
	const started = performance.now();
	const download = page.waitForEvent('download');
	await link.click();
	const path = await (await download).path();
	if (!path) throw new Error('The browser did not save the export.');
	const elapsedMs = performance.now() - started;
	const bytes = await readFile(path);
	return {
		elapsedMs,
		bytes: bytes.length,
		sha256: createHash('sha256').update(bytes).digest('hex')
	};
}

test.use({
	actionTimeout: 15_000,
	ignoreHTTPSErrors: true,
	trace: 'off',
	screenshot: 'off',
	video: 'off'
});

test('an Administrator shares a completed export over HTTPS and revokes the guest link', async ({
	page,
	browser
}, testInfo) => {
	test.setTimeout(300_000);
	const fixture = await startAuthenticationFixture(true);
	const guest = await browser.newContext({ ignoreHTTPSErrors: true });
	try {
		await fixture.command('administrator');
		await page.goto(fixture.origin);
		await page.getByRole('button', { name: 'Continue with Fixture sign-in' }).click();
		await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
		const start = fixture.guestRecordingStartMs;
		if (!Number.isSafeInteger(start) || start === null)
			throw new Error('The fixture must seed a real recording.');
		await page.goto(`${fixture.origin}/keep?camera=127.0.0.1&stream=main&at=${start}&mode=export`);
		await page.getByRole('button', { name: 'Create export', exact: true }).click();
		const panel = page.locator('[data-keep-export]');
		await expect(panel).toHaveAttribute('data-export-status', 'ready');
		const administratorDownload = await downloadExport(
			page,
			panel.getByRole('button', { name: 'Download', exact: true })
		);
		await panel.getByRole('button', { name: 'Create guest link' }).click();
		const dialog = page.getByRole('dialog', { name: 'Create guest link' });
		await dialog.getByLabel('Display name').fill('Entrance recording');
		await dialog.getByLabel('Allow download').check();
		await dialog.getByRole('button', { name: 'Create guest link' }).click();
		const url = await dialog.getByLabel('One-time guest link').inputValue();
		await dialog.getByRole('button', { name: 'Close', exact: true }).click();
		await page.goto(fixture.origin + '/settings#access');
		const viewer = await guest.newPage();
		const response = await viewer.goto(url).catch(() => {
			throw new Error('Could not open the one-time guest link.');
		});
		expect(response?.status()).toBe(200);
		expect(response?.headers()['cache-control']).toContain('no-store');
		expect(response?.headers()['referrer-policy']).toBe('no-referrer');
		await expect(viewer.getByRole('heading', { name: 'Entrance recording' })).toBeVisible();
		expect(new URL(viewer.url()).hash).toBe('');
		const video = viewer.locator('video');
		await video.evaluate(async (v) => {
			const media = v as HTMLVideoElement;
			media.muted = true;
			await media.play();
			media.pause();
			media.currentTime = media.duration / 2;
		});
		await expect
			.poll(() => video.evaluate((v) => (v as HTMLVideoElement).videoWidth))
			.toBeGreaterThan(0);
		await expect.poll(() => video.evaluate((v) => !(v as HTMLVideoElement).seeking)).toBe(true);
		const guestDownload = await downloadExport(
			viewer,
			viewer.getByRole('link', { name: 'Download MP4' })
		);
		expect(guestDownload.sha256).toBe(administratorDownload.sha256);
		const row = page
			.getByRole('listitem')
			.filter({ has: page.getByRole('heading', { name: 'Entrance recording' }) });
		await row.getByRole('button', { name: 'Revoke link' }).click();
		const revoke = page.getByRole('dialog', { name: 'Revoke guest access' });
		await revoke.getByLabel('Reason').fill('Review is complete.');
		const revokeStart = performance.now();
		await revoke.getByRole('button', { name: 'Confirm revocation' }).click();
		await expect(viewer.getByText('The link was revoked. Playback has stopped.')).toBeVisible();
		await expect(video).toHaveCount(0);
		await testInfo.attach('guest-export-timings', {
			contentType: 'application/json',
			body: JSON.stringify({
				bytes: guestDownload.bytes,
				administratorDownloadMs: administratorDownload.elapsedMs,
				guestDownloadMs: guestDownload.elapsedMs,
				guestRevokedUiMs: performance.now() - revokeStart
			})
		});
	} finally {
		await guest.close();
		await fixture.close();
	}
});
