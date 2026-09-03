import { expect, test } from '@playwright/test';

const eventId = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_ID');
const eventDate = requiredEnvironment('KEEPPEEK_CONFORMANCE_EVENT_DATE');
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
	await expect(detail.getByText('REVISION 1', { exact: true })).toBeVisible();
	await expect(detail.getByText('KeepPeek event pipeline', { exact: true })).toBeVisible();
	await expect(detail.locator('[data-event-bounding-box]')).toBeVisible();
	const image = detail.locator('[data-event-preview-image]');
	await expect(image).toBeVisible();
	await expect
		.poll(() =>
			image.evaluate((element: HTMLImageElement) => [element.naturalWidth, element.naturalHeight])
		)
		.toEqual([3840, 2160]);
});

function requiredEnvironment(name: string): string {
	const value = process.env[name];
	if (!value) throw new Error(`${name} is required`);
	return value;
}
