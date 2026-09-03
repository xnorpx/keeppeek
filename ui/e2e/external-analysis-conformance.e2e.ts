import { expect, test } from '@playwright/test';

const eventId = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_ID');
const eventDate = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_DATE');
const eventRevision = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_REVISION');
const eventTimestamp = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_TIMESTAMP');
const sourceId = requiredEnvironment('KEEPPEEK_CONFORMANCE_SOURCE_ID');

test('external conformance event is visible through normal query and UI', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/events?date=${eventDate}`);

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
});

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
