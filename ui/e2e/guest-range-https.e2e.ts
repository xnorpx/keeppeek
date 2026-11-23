import {
	expect,
	test,
	type BrowserContext,
	type Locator,
	type Page,
	type TestInfo
} from '@playwright/test';
import {
	startAuthenticationFixture,
	type AuthenticationFixture
} from './fixtures/external-authentication-server';

test.use({
	actionTimeout: 15_000,
	timezoneId: 'Pacific/Auckland',
	ignoreHTTPSErrors: true,
	trace: 'off',
	screenshot: 'off',
	video: 'off'
});

async function signInRangeOwnerAndGetStartMs(page: Page, fixture: AuthenticationFixture) {
	await fixture.command('administrator');
	await page.goto(fixture.origin);
	await page.getByRole('button', { name: 'Continue with Fixture sign-in' }).click();
	await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible({ timeout: 45_000 });
	const start = fixture.guestRecordingStartMs;
	if (start === null || !Number.isSafeInteger(start))
		throw new Error('The fixture requires retained video.');
	return start;
}

async function checkResponsivePreview(page: Page, dialog: Locator, testInfo: TestInfo) {
	for (const width of testInfo.repeatEachIndex === 0 ? [1440, 390, 320] : []) {
		await page.setViewportSize({ width, height: width === 1440 ? 900 : 844 });
		await expect
			.poll(() => dialog.evaluate((element) => element.scrollWidth <= element.clientWidth))
			.toBe(true);
		await testInfo.attach(`range-preview-${width}`, {
			body: await dialog.screenshot(),
			contentType: 'image/png'
		});
	}
}

function fixtureEventDate(recordingStartMs: number) {
	// authentication_browser_fixture.rs indexes guest-range-event 500 ms before the recording.
	// The event and the recording can belong to different UTC dates.
	return new Date(recordingStartMs - 500).toISOString().slice(0, 10);
}

async function openEventAndBookmark(
	page: Page,
	origin: string,
	start: number,
	dialog: Locator,
	selected: Locator
) {
	const date = fixtureEventDate(start);
	await page.goto(`${origin}/events?date=${date}`);
	const event = page.locator('[data-event-card="127.0.0.1:guest-range-event"]');
	await page
		.locator('[data-workflow-event="127.0.0.1:guest-range-event"]')
		.getByRole('button', { name: 'Bookmark event', exact: true })
		.click();
	await event.click();
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	const eventPreviewStart = performance.now();
	await detail.getByRole('button', { name: 'Share recording range' }).click();
	await expect(selected).toContainText(new Date(start - 500).toISOString());
	const eventPreviewMs = performance.now() - eventPreviewStart;
	const eventSelection = await selected.innerText();
	await dialog.getByLabel('Display name').press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(page.getByRole('complementary', { name: 'Event detail' })).toBeVisible();
	await page.getByRole('button', { name: 'Close event detail', exact: true }).click();
	await page.getByRole('button', { name: 'Saved bookmarks', exact: true }).click();
	const library = page.getByRole('dialog', { name: 'Saved bookmarks' });
	const bookmarkPreviewStart = performance.now();
	await library
		.locator('[data-saved-bookmark="127.0.0.1:guest-range-event"]')
		.getByRole('button', { name: 'Share recording range' })
		.click();
	await expect(selected).toHaveText(eventSelection);
	const bookmarkPreviewMs = performance.now() - bookmarkPreviewStart;
	return { eventPreviewMs, bookmarkPreviewMs };
}

async function playGuestEvidence(dialog: Locator, recipient: BrowserContext) {
	await dialog.getByLabel('Display name').fill('Selected recording');
	await dialog.getByRole('button', { name: 'Create guest link', exact: true }).click();
	const url = await dialog.getByLabel('One-time guest link').inputValue();
	const viewer = await recipient.newPage();
	await openRecipient(viewer, url);
	await expect(viewer.getByRole('heading', { name: 'Selected recording' })).toBeVisible();
	const target = await viewer.locator('video').evaluate(async (element) => {
		const video = element as HTMLVideoElement;
		video.muted = true;
		await video.play();
		video.pause();
		const middle = video.duration / 2;
		const seeked = new Promise<void>((resolve) =>
			video.addEventListener('seeked', () => resolve(), { once: true })
		);
		video.currentTime = middle;
		await seeked;
		return middle;
	});
	await expect
		.poll(() =>
			viewer.locator('video').evaluate((video, expected) => {
				const player = video as HTMLVideoElement;
				return (
					!player.seeking &&
					player.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA &&
					player.videoWidth > 0 &&
					Math.abs(player.currentTime - expected) < 0.05
				);
			}, target)
		)
		.toBe(true);
	await expect(viewer.getByRole('link', { name: 'Download MP4' })).toHaveCount(0);
}

test('the selected Keep, event and bookmark intervals reach the same guarded HTTPS creation flow', async ({
	page,
	browser
}, testInfo) => {
	test.setTimeout(180_000);
	const fixture = await startAuthenticationFixture(true);
	const recipient = await browser.newContext({ ignoreHTTPSErrors: true });
	try {
		await page.setViewportSize({ width: 1440, height: 900 });
		const start = await signInRangeOwnerAndGetStartMs(page, fixture);
		await page.goto(`${fixture.origin}/keep?camera=127.0.0.1&stream=sub&at=${start}&mode=export`);
		const panel = page.locator('[data-keep-export]');
		const dialog = page.getByRole('dialog', { name: 'Create guest link', exact: true });
		await panel.getByRole('button', { name: 'Create export', exact: true }).click();
		await expect(panel).toHaveAttribute('data-export-status', 'ready');
		await expect(panel.getByRole('button', { name: 'Share recording range' })).toBeVisible();
		const readyStart = performance.now();
		await panel.getByRole('button', { name: 'Create guest link', exact: true }).click();
		await expect(dialog.getByLabel('Display name')).toBeVisible();
		const readyPreviewMs = performance.now() - readyStart;
		await dialog.getByRole('button', { name: 'Close guest sharing' }).click();
		await page.reload();
		const rangeButton = panel.getByRole('button', { name: 'Share recording range' });
		await expect(rangeButton).toBeVisible();
		const keepPreviewStart = performance.now();
		await rangeButton.click();
		const selected = dialog.getByText(/Selected interval/);
		await expect(selected).toBeVisible();
		const keepPreviewMs = performance.now() - keepPreviewStart;
		const keepStart = Number(await panel.getAttribute('data-export-start-ms'));
		const keepEnd = Number(await panel.getAttribute('data-export-end-ms'));
		await expect(selected).toContainText(new Date(keepStart).toISOString());
		await expect(selected).toContainText(new Date(keepEnd).toISOString());
		await checkResponsivePreview(page, dialog, testInfo);
		await dialog.getByRole('button', { name: 'Close guest sharing' }).click();
		await page.setViewportSize({ width: 1440, height: 900 });
		const { eventPreviewMs, bookmarkPreviewMs } = await openEventAndBookmark(
			page,
			fixture.origin,
			start,
			dialog,
			selected
		);
		await testInfo.attach('range-preview-timings', {
			contentType: 'application/json',
			body: JSON.stringify({ readyPreviewMs, keepPreviewMs, eventPreviewMs, bookmarkPreviewMs })
		});
		await playGuestEvidence(dialog, recipient);
	} finally {
		await recipient.close();
		await fixture.close();
	}
});

async function openCreatorRange(
	page: Page,
	origin: string,
	start: number,
	entry: 'Keep' | 'Event'
) {
	if (entry === 'Keep') {
		await page.goto(`${origin}/keep?camera=127.0.0.1&stream=sub&at=${start}&mode=export`);
		const panel = page.locator('[data-keep-export]');
		await panel.getByRole('button', { name: 'Share recording range' }).click();
		const startText = await panel.getAttribute('data-export-start-ms');
		const endText = await panel.getAttribute('data-export-end-ms');
		if (!startText?.trim() || !endText?.trim()) throw new Error('Keep export bounds are missing.');
		const from = Number(startText);
		const to = Number(endText);
		if (!Number.isSafeInteger(from) || !Number.isSafeInteger(to) || from >= to)
			throw new Error('Keep export bounds are invalid.');
		return { start: from, end: to };
	}
	const date = fixtureEventDate(start);
	await page.goto(`${origin}/events?date=${date}`);
	await page.locator('[data-event-card="127.0.0.1:guest-range-event"]').click();
	await page
		.getByRole('complementary', { name: 'Event detail' })
		.getByRole('button', { name: 'Share recording range' })
		.click();
	const selected = page
		.getByRole('dialog', { name: 'Create guest link', exact: true })
		.getByText(/Selected interval/);
	await expect(selected).toBeVisible();
	const dates = (await selected.innerText()).match(
		/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z/g
	);
	if (dates?.length !== 2) throw new Error('The event preview must show both interval edges.');
	const from = Date.parse(dates[0]!);
	const to = Date.parse(dates[1]!);
	if (!Number.isSafeInteger(from) || !Number.isSafeInteger(to) || from >= to)
		throw new Error('The event preview must show an ordered UTC interval.');
	return { start: from, end: to };
}

async function openRecipient(page: Page, url: string) {
	try {
		await page.goto(url);
	} catch (error) {
		// Playwright's raw error and cause include the one-time bearer URL.
		const details = error instanceof Error ? error.message : '';
		const code = [
			'ERR_CONNECTION_REFUSED',
			'ERR_CONNECTION_RESET',
			'ERR_NAME_NOT_RESOLVED',
			'ERR_CERT_AUTHORITY_INVALID',
			'ERR_CERT_DATE_INVALID',
			'ERR_TIMED_OUT',
			'ERR_ABORTED'
		].find((value) => details.includes(`net::${value}`));
		const reason =
			code ??
			(error instanceof Error && error.name === 'TimeoutError' ? 'timeout' : 'navigation error');
		throw new Error(`The guest evidence page did not load (${reason}).`);
	}
}

function recorderUtc(header: string | null | undefined) {
	if (!header) throw new Error('The recorder response omitted its HTTP Date.');
	const utc = Date.parse(header);
	if (!Number.isFinite(utc)) throw new Error('The recorder returned an invalid HTTP Date.');
	return utc;
}

async function verifyGuestExpiresDuringPlayback(
	viewer: Page,
	url: string,
	expiry: number,
	exchangeUtc: number
) {
	const timeout = Math.max(1, Math.min(75_000, expiry - exchangeUtc + 15_000));
	const player = await viewer
		.locator('video')
		.evaluateHandle((element) => element as HTMLVideoElement);
	const initialTime = await player.evaluate(async (video) => {
		video.muted = true;
		video.loop = true;
		await video.play();
		return video.currentTime;
	});
	await expect
		.poll(
			() =>
				player.evaluate(
					(video, initial) =>
						video.videoWidth > 0 &&
						video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA &&
						!video.paused &&
						video.currentTime > initial + 0.05,
					initialTime
				),
			{ timeout }
		)
		.toBe(true);
	const expired = await viewer.waitForResponse(
		async (response) => {
			if (
				response.request().method() !== 'GET' ||
				new URL(response.url()).pathname !== '/guest/session' ||
				!response.ok()
			)
				return false;
			const body: unknown = await response.json().catch(() => null);
			return (
				typeof body === 'object' && body !== null && 'status' in body && body.status === 'expired'
			);
		},
		{ timeout }
	);
	// The grant is aligned to a whole UTC second, the precision of the recorder's HTTP Date.
	expect(recorderUtc(await expired.headerValue('date'))).toBeGreaterThanOrEqual(expiry);
	await expect(viewer.getByText('The link expired. Playback has stopped.')).toBeVisible({
		timeout
	});
	await expect(viewer.locator('video')).toHaveCount(0, { timeout });
	// clearMedia pauses the retained element and removes its source before Svelte detaches it.
	expect(
		await player.evaluate(
			(video) => video.paused && !video.isConnected && !video.hasAttribute('src')
		)
	).toBe(true);
	await player.dispose();
	const recipient = viewer.context();
	await recipient.clearCookies();
	const fresh = await recipient.newPage();
	await openRecipient(fresh, url);
	await expect(fresh.getByRole('heading', { name: 'This link is unavailable.' })).toBeVisible();
	await expect(fresh.locator('video')).toHaveCount(0, { timeout });
}

for (const entry of ['Keep', 'Event'] as const) {
	test(`${entry} issues a bounded HTTPS grant with the chosen expiry and download setting`, async ({
		page,
		browser
	}) => {
		test.setTimeout(150_000);
		const fixture = await startAuthenticationFixture(true);
		const recipient = await browser.newContext({ ignoreHTTPSErrors: true });
		try {
			const start = await signInRangeOwnerAndGetStartMs(page, fixture);
			const selection = await openCreatorRange(page, fixture.origin, start, entry);
			const dialog = page.getByRole('dialog', { name: 'Create guest link', exact: true });
			const selected = dialog.getByText(/Selected interval/);
			for (const boundary of [selection.start, selection.end]) {
				await expect(selected).toContainText(new Date(boundary).toISOString());
			}
			const download = entry === 'Event';
			await dialog.getByLabel('Display name').fill('Time-limited recording');
			await dialog.getByLabel('Allow download').setChecked(download);
			const clock = await page.request.get(fixture.origin + '/auth/session', { timeout: 10_000 });
			expect(clock.ok()).toBe(true);
			const expiry = recorderUtc(clock.headers().date) + (download ? 180_000 : 45_000);
			await dialog.getByLabel('Expires (UTC)').fill(new Date(expiry).toISOString().slice(0, 19));
			await dialog.getByRole('button', { name: 'Create guest link', exact: true }).click();
			const url = await dialog.getByLabel('One-time guest link').inputValue();
			const viewer = await recipient.newPage();
			const session = viewer.waitForResponse(
				(response) =>
					response.ok() &&
					response.request().method() === 'POST' &&
					new URL(response.url()).pathname === '/guest/session'
			);
			await openRecipient(viewer, url);
			await expect(viewer.getByRole('heading', { name: 'Time-limited recording' })).toBeVisible();
			const exchange = await session;
			const metadata = await exchange.json();
			expect(metadata.expires_at_ms).toBe(expiry);
			expect(metadata.can_stream).toBe(true);
			expect(metadata.can_download).toBe(download);
			expect(metadata.start_ms).toBeGreaterThanOrEqual(selection.start);
			expect(metadata.end_ms).toBeLessThanOrEqual(selection.end);
			expect(metadata.end_ms).toBeGreaterThan(metadata.start_ms);
			await expect(viewer.getByRole('link', { name: 'Download MP4' })).toHaveCount(
				download ? 1 : 0
			);
			if (download) {
				const ordinary = await recipient.newPage();
				await ordinary.goto(fixture.origin);
				await expect(
					ordinary.getByRole('button', { name: 'Continue with Fixture sign-in' })
				).toBeVisible();
				await expect(ordinary.getByRole('button', { name: 'Sign out' })).toHaveCount(0);
			} else {
				await verifyGuestExpiresDuringPlayback(
					viewer,
					url,
					expiry,
					recorderUtc(await exchange.headerValue('date'))
				);
			}
		} finally {
			await recipient.close();
			await fixture.close();
		}
	});
}
