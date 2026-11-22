import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { expect, test, type BrowserContext, type Locator, type Page } from '@playwright/test';
import { startAuthenticationFixture } from './fixtures/external-authentication-server';

function observeGuestNetwork(context: BrowserContext, origin: string, token: string) {
	const paths = new Set<string>();
	const checks: Promise<void>[] = [];
	const problems = new Set<string>();
	context.on('request', (request) => {
		if (checks.length >= 512) {
			problems.add('guest request observation limit exceeded');
			return;
		}
		const destination = new URL(request.url());
		if (destination.origin !== origin) problems.add('third-party request');
		if ((destination.pathname + destination.search).includes(token))
			problems.add('token in request URL');
		checks.push(
			request.allHeaders().then(
				(headers) => {
					if (headers.referer?.includes(token)) problems.add('token in referrer');
				},
				() => {
					problems.add('unreadable request headers');
				}
			)
		);
	});
	context.on('response', (response) => {
		const path = new URL(response.url()).pathname;
		if (path !== '/share' && !path.startsWith('/guest/')) return;
		paths.add(path);
		const headers = response.headers();
		if (
			headers['cache-control'] !== 'private, no-store' ||
			headers['referrer-policy'] !== 'no-referrer' ||
			headers['x-content-type-options'] !== 'nosniff' ||
			headers['x-frame-options'] !== 'DENY' ||
			headers['cross-origin-resource-policy'] !== 'same-origin' ||
			!headers['content-security-policy']?.includes("frame-ancestors 'none'")
		) {
			problems.add('missing guest response protection');
		}
	});
	return async () => {
		await Promise.all(checks);
		expect([...problems]).toEqual([]);
		expect([...paths].sort()).toEqual([
			'/guest/download',
			'/guest/media',
			'/guest/session',
			'/share'
		]);
	};
}

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

type Fixture = Awaited<ReturnType<typeof startAuthenticationFixture>>;

async function createLink(page: Page, fixture: Fixture) {
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
	return { url, administratorDownload };
}

function observePrivateConsole(page: Page, values: string[]) {
	let disclosed = false;
	page.on('console', (message) => {
		disclosed ||= values.some((value) => message.text().includes(value));
	});
	return () => expect(disclosed).toBe(false);
}

async function verifyPrivatePage(page: Page, values: string[], verifyConsole: () => void) {
	verifyConsole();
	expect(new URL(page.url()).hash).toBe('');
	expect(
		await page.evaluate((secrets) => {
			const state = JSON.stringify([localStorage, sessionStorage, history.state, location.href]);
			return secrets.some((value) => state.includes(value));
		}, values)
	).toBe(false);
}

async function playAndSeek(viewer: Page) {
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
}

async function firstDownloadChunk(viewer: Page, expectedBytes: number) {
	const transfer = await viewer
		.getByRole('link', { name: 'Download MP4' })
		.evaluate(async (element, expectedBytes) => {
			const started = performance.now();
			const response = await fetch((element as HTMLAnchorElement).href);
			if (!response.ok || !response.body) throw new Error('Guest download could not be inspected.');
			const reader = response.body.getReader();
			let bytes = 0;
			let firstByteMs: number | null = null;
			for (;;) {
				const next = await reader.read();
				if (next.done) break;
				if (next.value.byteLength > 0) firstByteMs ??= performance.now() - started;
				bytes += next.value.byteLength;
				if (bytes > expectedBytes) {
					await reader.cancel();
					throw new Error('Guest download exceeded the evidence size.');
				}
			}
			return { bytes, firstByteMs };
		}, expectedBytes);
	expect(transfer.bytes).toBe(expectedBytes);
	expect(transfer.firstByteMs).not.toBeNull();
	return transfer.firstByteMs;
}

async function revoke(page: Page, viewer: Page) {
	const row = page
		.getByRole('listitem')
		.filter({ has: page.getByRole('heading', { name: 'Entrance recording' }) });
	await row.getByRole('button', { name: 'Revoke link' }).click();
	const revoke = page.getByRole('dialog', { name: 'Revoke guest access' });
	await revoke.getByLabel('Reason').fill('Review is complete.');
	const revokeStart = performance.now();
	await revoke.getByRole('button', { name: 'Confirm revocation' }).click();
	await expect(viewer.getByText('The link was revoked. Playback has stopped.')).toBeVisible();
	await expect(viewer.locator('video')).toHaveCount(0);
	return performance.now() - revokeStart;
}

async function openGuest(page: Page, url: string) {
	const response = await page.goto(url).catch(() => {
		throw new Error('Could not open the guest link.');
	});
	expect(response?.status()).toBe(200);
	expect(response?.headers()['cache-control']).toContain('no-store');
	expect(response?.headers()['referrer-policy']).toBe('no-referrer');
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
		const { url, administratorDownload } = await createLink(page, fixture);
		const token = new URL(url).hash.slice(1);
		expect(token.length).toBe(43);
		const verifyNetwork = observeGuestNetwork(guest, fixture.origin, token);
		const privateValues = [token];
		const viewer = await guest.newPage();
		const verifyViewerConsole = observePrivateConsole(viewer, privateValues);
		await openGuest(viewer, url);
		await expect(viewer.getByRole('heading', { name: 'Entrance recording' })).toBeVisible();
		const cookies = await guest.cookies(fixture.origin);
		expect(
			cookies.map(({ name, secure, httpOnly, sameSite }) => ({ name, secure, httpOnly, sameSite }))
		).toEqual([
			{ name: '__Host-keeppeek-guest', secure: true, httpOnly: true, sameSite: 'Strict' }
		]);
		privateValues.push(cookies[0].value);
		await verifyPrivatePage(viewer, privateValues, verifyViewerConsole);
		await playAndSeek(viewer);
		const guestDownload = await downloadExport(
			viewer,
			viewer.getByRole('link', { name: 'Download MP4' })
		);
		expect(guestDownload.sha256).toBe(administratorDownload.sha256);
		const firstByteMs = await firstDownloadChunk(viewer, guestDownload.bytes);
		const revokedUiMs = await revoke(page, viewer);
		await verifyPrivatePage(viewer, privateValues, verifyViewerConsole);
		await guest.clearCookies();
		const reopened = await guest.newPage();
		const verifyReopenedConsole = observePrivateConsole(reopened, privateValues);
		await openGuest(reopened, url);
		await expect(
			reopened.getByRole('heading', { name: 'This link is unavailable.' })
		).toBeVisible();
		await expect(reopened.locator('video')).toHaveCount(0);
		await verifyPrivatePage(reopened, privateValues, verifyReopenedConsole);
		await verifyNetwork();
		const audit = JSON.stringify(await fixture.audit());
		expect(privateValues.some((value) => audit.includes(value))).toBe(false);
		await testInfo.attach('guest-export-timings', {
			contentType: 'application/json',
			body: JSON.stringify({
				bytes: guestDownload.bytes,
				administratorDownloadMs: administratorDownload.elapsedMs,
				guestDownloadMs: guestDownload.elapsedMs,
				guestDownloadFirstByteMs: firstByteMs,
				guestRevokedUiMs: revokedUiMs
			})
		});
	} finally {
		await guest.close();
		await fixture.close();
	}
});
