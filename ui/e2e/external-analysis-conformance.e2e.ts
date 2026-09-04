import { expect, test } from '@playwright/test';

const eventId = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_ID');
const eventDate = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_DATE');
const eventRevision = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_REVISION');
const eventTimestamp = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_TIMESTAMP');
const sourceId = requiredEnvironment('KEEPPEEK_CONFORMANCE_SOURCE_ID');
const diagnosticEntryLimit = 16;
const diagnosticTextLimit = 1_000;

test('external conformance event is visible through normal query and UI', async ({ page }) => {
	const browserErrors: string[] = [];
	const requestFailures: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') appendDiagnostic(browserErrors, message.text());
	});
	page.on('pageerror', (error) => appendDiagnostic(browserErrors, error.message));
	page.on('requestfailed', (request) => {
		appendDiagnostic(
			requestFailures,
			`${request.method()} ${new URL(request.url()).pathname}: ${request.failure()?.errorText ?? 'unknown error'}`
		);
	});
	const createResponse = page
		.waitForResponse(
			(response) => response.url().endsWith('/create') && response.request().method() === 'POST',
			{ timeout: 15_000 }
		)
		.catch(() => null);
	await page.setViewportSize({ width: 1440, height: 900 });
	const response = await page.goto(`/events?date=${eventDate}`);
	expect(response?.status()).toBe(200);
	const created = await createResponse;
	if (created === null) {
		throw new Error(
			`Browser did not create a WebRTC control session: ${JSON.stringify({
				title: await page.title(),
				body: (await page.locator('body').innerText()).slice(0, diagnosticTextLimit),
				browserErrors,
				requestFailures
			})}`
		);
	}
	expect(created.status()).toBe(201);
	await expect(page).toHaveTitle('Events - KeepPeek');

	const card = page.locator(`[data-event-card="${sourceId}:${eventId}"]`);
	await expect(card).toBeVisible();
	await expect(card).toContainText('Person');
	await expect(card).toContainText('0.90');
	await card.click();

	const detail = page.getByRole('complementary', { name: 'Event detail' });
	await expect(detail).toBeVisible();
	await expect(detail.getByText(`REVISION ${eventRevision}`, { exact: true })).toBeVisible();
	await expect(detail.getByText('KeepPeek event pipeline', { exact: true })).toBeVisible();
	await expect(detail.locator('[data-event-bounding-box]')).toBeVisible();
	const image = detail.locator('[data-event-preview-image]');
	await expect(image).toBeVisible();
	await expect
		.poll(() =>
			image.evaluate((element: HTMLImageElement) => [element.naturalWidth, element.naturalHeight])
		)
		.toEqual([3840, 2160]);

	await page.goto('/');
	const liveView = page.locator(`[data-camera-id="${sourceId}"]`);
	await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
	const liveVideo = liveView.locator('video');
	await expect(liveVideo).toBeVisible();
	const liveFrames = await decodedFrames(liveVideo);
	await expect
		.poll(() => decodedFrames(liveVideo), { timeout: 15_000 })
		.toBeGreaterThan(liveFrames);

	const eventTimestampMs = Date.parse(eventTimestamp);
	expect(eventTimestampMs).not.toBeNaN();
	await page.goto(`/keep?camera=${sourceId}&stream=sub&date=${eventDate}&at=${eventTimestampMs}`);
	const player = page.locator('[data-keep-player]');
	await expect(player).toHaveAttribute('data-recording-startup-phase', 'first-frame', {
		timeout: 30_000
	});
	const recordedVideo = player.locator('video');
	await expect(recordedVideo).toBeVisible();
	await expect.poll(() => decodedFrames(recordedVideo), { timeout: 15_000 }).toBeGreaterThan(0);
	expect(browserErrors).toEqual([]);
	expect(requestFailures).toEqual([]);
});

function appendDiagnostic(entries: string[], value: string): void {
	if (entries.length < diagnosticEntryLimit) entries.push(value.slice(0, diagnosticTextLimit));
}

async function decodedFrames(locator: import('@playwright/test').Locator): Promise<number> {
	return locator.evaluate(
		(element) => (element as HTMLVideoElement).getVideoPlaybackQuality().totalVideoFrames
	);
}

function requiredEnvironment(name: string): string {
	const value = process.env[name];
	if (!value) throw new Error(`${name} is required`);
	return value;
}
