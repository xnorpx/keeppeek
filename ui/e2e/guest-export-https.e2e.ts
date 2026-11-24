import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { gzipSync } from 'node:zlib';
import {
	expect,
	test,
	type APIResponse,
	type BrowserContext,
	type Locator,
	type Page,
	type Request,
	type Response
} from '@playwright/test';
import { startAuthenticationFixture } from './fixtures/external-authentication-server';

function observeGuestNetwork(context: BrowserContext, origin: string, privateValues: string[]) {
	const routes = ['/guest/download', '/guest/media', '/guest/session', '/share'];
	const observed: string[] = [];
	const paths = new Set<string>();
	const checks: Promise<void>[] = [];
	const problems = new Set<string>();
	function record(value: string) {
		if (value.length > 8192) problems.add('guest request observation exceeded the length limit');
		else observed.push(value);
	}
	const onRequest = (request: Request) => {
		if (checks.length >= 512) {
			problems.add('guest request observation limit exceeded');
			return;
		}
		const destination = new URL(request.url());
		if (destination.origin !== origin) problems.add('third-party request');
		record(destination.pathname + destination.search);
		checks.push(
			request.headerValue('referer').then(
				(referer) => {
					if (referer) record(referer);
				},
				() => {
					problems.add('unreadable request referrer');
				}
			)
		);
	};
	const onResponse = (response: Response) => {
		const path = new URL(response.url()).pathname;
		if (path !== '/share' && !path.startsWith('/guest/')) return;
		if (routes.includes(path)) paths.add(path);
		else problems.add('unexpected guest endpoint');
		const headers = response.headers();
		const directives = new Set(
			headers['cache-control']?.split(',').map((directive) => directive.trim().toLowerCase())
		);
		if (
			!directives.has('private') ||
			!directives.has('no-store') ||
			headers['referrer-policy'] !== 'no-referrer' ||
			headers['x-content-type-options']?.toLowerCase() !== 'nosniff' ||
			headers['x-frame-options']?.toLowerCase() !== 'deny' ||
			headers['cross-origin-resource-policy'] !== 'same-origin' ||
			!headers['content-security-policy']?.includes("frame-ancestors 'none'")
		) {
			problems.add('missing guest response protection');
		}
	};
	context.on('request', onRequest);
	context.on('response', onResponse);
	return async () => {
		context.off('request', onRequest);
		context.off('response', onResponse);
		await Promise.all(checks);
		expect([...problems]).toEqual([]);
		expect(observed.some((value) => privateValues.some((secret) => value.includes(secret)))).toBe(
			false
		);
		expect([...paths].sort()).toEqual(routes);
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
	await page.goto(`${fixture.origin}/keep?camera=127.0.0.1&stream=sub&at=${start}&mode=export`);
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
	return { url, administratorDownload, start };
}

function observePrivateConsole(page: Page, values: string[]) {
	const entries: string[] = [];
	let overflow = false;
	function record(text: string) {
		if (entries.length >= 128 || text.length > 8192) {
			overflow = true;
			return;
		}
		entries.push(text);
	}
	page.on('console', (message) => record(message.text()));
	page.on('pageerror', (error) => record(error.stack ?? error.message));
	return () => {
		expect(overflow).toBe(false);
		expect(entries.some((entry) => values.some((value) => entry.includes(value)))).toBe(false);
	};
}

async function verifyPrivatePage(page: Page, values: string[], verifyConsole: () => void) {
	verifyConsole();
	expect(new URL(page.url()).hash).toBe('');
	expect(
		await page.evaluate((secrets) => {
			const entries = (storage: Storage) =>
				Array.from({ length: storage.length }, (_, index) => {
					const key = storage.key(index);
					return key === null ? [] : [key, storage.getItem(key)];
				});
			const state = JSON.stringify([
				entries(localStorage),
				entries(sessionStorage),
				history.state,
				location.href
			]);
			return secrets.some((value) => state.includes(value));
		}, values)
	).toBe(false);
}

async function playAndSeek(viewer: Page) {
	const video = viewer.locator('video');
	const target = await video.evaluate(async (element) => {
		const media = element as HTMLVideoElement;
		media.muted = true;
		await media.play();
		media.pause();
		const target = media.duration / 2;
		media.currentTime = target;
		return target;
	});
	expect(target).toBeGreaterThan(0);
	await expect
		.poll(() =>
			video.evaluate((element, target) => {
				const media = element as HTMLVideoElement;
				return (
					!media.seeking &&
					media.paused &&
					media.videoWidth > 0 &&
					media.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA &&
					Math.abs(media.currentTime - target) < 0.05
				);
			}, target)
		)
		.toBe(true);
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
	const dialog = page.getByRole('dialog', { name: 'Revoke guest access' });
	await dialog.getByLabel('Reason').fill('Review is complete.');
	const revokeStart = performance.now();
	await dialog.getByRole('button', { name: 'Confirm revocation' }).click();
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
		const privateValues = [token];
		const verifyNetwork = observeGuestNetwork(guest, fixture.origin, privateValues);
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

test('a recipient can play the shared export while ordinary recorder surfaces stay signed out', async ({
	page,
	browser
}) => {
	test.setTimeout(180_000);
	const fixture = await startAuthenticationFixture(true);
	const recipient = await browser.newContext({ ignoreHTTPSErrors: true });
	try {
		const { url, start, administratorDownload } = await createLink(page, fixture);
		const viewer = await recipient.newPage();
		await openGuest(viewer, url);
		await expect(viewer.getByRole('heading', { name: 'Entrance recording' })).toBeVisible();
		await playAndSeek(viewer);
		const mediaUrl = new URL(
			await viewer.locator('video').evaluate((element: HTMLVideoElement) => element.currentSrc)
		);
		expect(mediaUrl.origin).toBe(fixture.origin);
		expect(mediaUrl.pathname).toBe('/guest/media');
		const ordinary = await recipient.newPage();
		const routes = [
			['Live, snapshots, PTZ and talk', '/?camera=127.0.0.1'],
			['Camera inventory', '/cameras'],
			['Another camera', '/camera?camera=127.0.0.2'],
			['Adjacent recording', `/keep?camera=127.0.0.1&at=${start - 1}`],
			['Event search', '/events'],
			['Health', '/system-health'],
			['Logs', '/settings/logs'],
			['Users and settings', '/settings#access']
		] as const;
		for (const [surface, path] of routes) {
			await test.step(surface, async () => {
				await ordinary.goto(fixture.origin + path);
				await expect(ordinary.getByRole('heading', { name: 'Sign in', exact: true })).toBeVisible();
				await expect(
					ordinary.locator(
						'video, audio, [data-event-card], [data-keep-player], [data-health-metric]'
					)
				).toHaveCount(0);
				await expect(
					ordinary.getByRole('button', {
						name: /^(Pan left|Talk(?: to .+)?|Broadcast talkback|Snapshot)$/i
					})
				).toHaveCount(0);
				await expect(ordinary.locator('body')).not.toContainText(
					/Front Door|guest-range-event|Entrance recording/
				);
			});
		}
		const headers = { Origin: fixture.origin };
		const session = await ordinary.request.get(fixture.origin + '/auth/session', { headers });
		expect(session.status()).toBe(200);
		expect((await session.json()).identity === null).toBe(true);
		const adjacent = new URLSearchParams({ start_ms: String(start - 1000), end_ms: String(start) });
		for (const path of [
			'/logs/snapshot',
			'/metrics',
			'/config/export',
			`/recording-coverage?${adjacent}`
		]) {
			await test.step(new URL(path, fixture.origin).pathname, async () => {
				await expectSignInRequired(await ordinary.request.get(fixture.origin + path, { headers }));
			});
		}
		await test.step('Ordinary media and control connection', async () => {
			const offer = await ordinary.evaluate(async () => {
				const peer = new RTCPeerConnection({ iceServers: [] });
				try {
					peer.createDataChannel('control');
					return await peer.createOffer();
				} finally {
					peer.close();
				}
			});
			await expectSignInRequired(
				await ordinary.request.post(fixture.origin + '/create', {
					headers: { ...headers, 'Content-Type': 'application/json', 'Content-Encoding': 'gzip' },
					data: gzipSync(JSON.stringify({ offer }))
				})
			);
		});
		const names = (await recipient.cookies(fixture.origin)).map(({ name }) => name);
		expect(names).toContain('__Host-keeppeek-guest');
		expect(names).not.toContain('__Host-keeppeek-session');
		const media = await recipient.request.get(mediaUrl.href, { maxRedirects: 0 });
		expect(media.status()).toBe(200);
		expect(media.headers()['content-type']).toBe('video/mp4');
		const bytes = await media.body();
		expect(bytes.length).toBe(administratorDownload.bytes);
		expect(createHash('sha256').update(bytes).digest('hex')).toBe(administratorDownload.sha256);
	} finally {
		await recipient.close();
		await fixture.close();
	}
});

async function expectSignInRequired(response: APIResponse) {
	expect(response.status()).toBe(401);
	expect(await response.json()).toEqual({
		code: 401,
		message: expect.stringMatching(/sign.in|access key|authentication/i)
	});
}
