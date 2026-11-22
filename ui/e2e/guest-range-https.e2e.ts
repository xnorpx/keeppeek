import {
	expect,
	test,
	type BrowserContext,
	type Locator,
	type Page,
	type TestInfo
} from '@playwright/test';
import { startAuthenticationFixture } from './fixtures/external-authentication-server';

test.use({
	actionTimeout: 15_000,
	ignoreHTTPSErrors: true,
	trace: 'off',
	screenshot: 'off',
	video: 'off'
});

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

async function openEventAndBookmark(
	page: Page,
	origin: string,
	start: number,
	dialog: Locator,
	selected: Locator
) {
	const date = new Date(start - 500).toISOString().slice(0, 10);
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
	await dialog.getByRole('button', { name: 'Close guest sharing' }).click();
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
	await viewer.goto(url).catch(() => {
		throw new Error('The guest page did not load.');
	});
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
		await fixture.command('administrator');
		await page.goto(fixture.origin);
		await page.getByRole('button', { name: 'Continue with Fixture sign-in' }).click();
		// Cold recorder sign-in precedes the timed evidence previews.
		await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible({ timeout: 45_000 });
		const start = fixture.guestRecordingStartMs;
		if (start === null || !Number.isSafeInteger(start))
			throw new Error('The fixture requires retained video.');
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
