import { expect, test } from '@playwright/test';
import type { CameraListItem, RecordingEvent } from '../src/lib/types';
import { mockControlPeer, type StoredEventFixture } from './fixtures/control-peer';
import {
	eventDate,
	mockEvents,
	mockEventsWithOlderFilteredMatch,
	mockEventsWithUnavailablePreview
} from './fixtures/events';

test('Board 10 browse restores URL filters and preserves mixed Event states', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	await mockEvents(page);
	await page.goto(`/events?type=person&date=${eventDate}`);

	await expect(page).toHaveTitle('Events - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Events', exact: true })).toBeVisible();
	await expect(page.getByLabel('Event type filter')).toHaveValue('person');
	await expect(page.locator('[data-event-card]')).toHaveCount(2);
	await expect(page.locator('[data-event-card]').first().locator('time')).toContainText('UTC');
	await expect(page.getByText('0.94', { exact: true })).toBeVisible();
	await expect(page.getByText('0.42', { exact: true })).toBeVisible();

	await page.getByLabel('Minimum confidence').fill('0.8');
	await page.getByLabel('Minimum confidence').press('Tab');
	await expect(page).toHaveURL(/confidence=0.8/);
	await expect(page.locator('[data-event-card]')).toHaveCount(1);
	await page.getByRole('button', { name: 'Clear filters' }).click();
	await expect(page.locator('[data-event-card]')).toHaveCount(5);
	await expect(page.getByText('NO IMAGE', { exact: true })).toBeVisible();
	await expect(page.getByText('STORY', { exact: true })).toBeVisible();

	await page.getByLabel('Image filter').selectOption('without');
	await expect(page).toHaveURL(/image=without/);
	await expect(page.locator('[data-event-card]')).toHaveCount(1);
	await expect(page.locator('[data-event-card="front-door:motion-no-image"]')).toBeVisible();
	await page.getByRole('button', { name: 'Clear filters' }).click();
	await page.getByRole('searchbox', { name: 'Search events' }).fill('nothing-here');
	await expect(page.getByText('No events found.')).toBeVisible();
	await expect(
		page.getByText('No events found for matching “nothing-here” on 2026-08-18.')
	).toBeVisible();
	await page.getByRole('button', { name: 'Clear “nothing-here” · 5 results' }).click();
	await expect(page.locator('[data-event-card]')).toHaveCount(5);
	await expect(page).toHaveURL(new RegExp(`/events\\?date=${eventDate}$`));
});

test('continues a filtered Events search into an earlier bounded window', async ({ page }) => {
	await mockEventsWithOlderFilteredMatch(page);
	await page.goto(`/events?date=${eventDate}&type=person`);

	await expect(page.locator('[data-event-card]')).toHaveCount(0);
	await expect(page.getByText('No matching events in the loaded window.')).toBeVisible();
	await page.getByRole('button', { name: 'Search earlier events' }).click();

	await expect(page.locator('[data-event-card="front-door:older-person"]')).toBeVisible();
	await expect(page.getByText('1-1 of 1 loaded')).toBeVisible();

	await page.getByRole('button', { name: 'Clear filters' }).click();
	const visibleCards = page.locator('[data-event-card]');
	await expect(visibleCards).toHaveCount(18);
	const lastVisibleCard = visibleCards.last();
	await lastVisibleCard.focus();
	await lastVisibleCard.press('ArrowDown');
	await expect(lastVisibleCard).toBeFocused();
	await expect(lastVisibleCard).toHaveAttribute('tabindex', '0');
});

test('cancels an earlier Events search cleanly when the date changes', async ({ page }) => {
	let releaseEarlierSearch!: () => void;
	const earlierSearchGate = new Promise<void>((resolve) => {
		releaseEarlierSearch = resolve;
	});
	await mockEventsWithOlderFilteredMatch(page, [Promise.resolve(), earlierSearchGate]);
	await page.goto(`/events?date=${eventDate}&type=person`);

	await page.getByRole('button', { name: 'Search earlier events' }).click();
	await page.getByLabel('Event date').fill('2026-08-19');
	await page.getByLabel('Event date').press('Tab');
	releaseEarlierSearch();

	await expect(page).toHaveURL(/date=2026-08-19/);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByText('No events found.')).toBeVisible();
});

test('refreshes today’s event metadata without clearing rendered results', async ({ page }) => {
	await page.clock.install();
	const camera = {
		id: 'front-door',
		ip: '192.0.2.1',
		name: 'Front Door',
		manufacturer: 'Reolink',
		model: 'RLC-811A',
		firmware_version: null,
		is_reolink: true,
		profiles: []
	} satisfies CameraListItem;
	const storedEvents: StoredEventFixture[] = [];
	const requests = await mockControlPeer(page, { cameras: [camera], storedEvents });
	const today = new Date().toISOString().slice(0, 10);
	await page.goto(`/events?date=${today}`);

	await expect(page.getByText('LIVE', { exact: true })).toBeVisible();
	await expect(page.getByText('No events found.', { exact: true })).toBeVisible();
	const initialQueryCount = requests.storedTimelineQueries.length;
	storedEvents.push({
		sourceId: camera.id,
		event: {
			id: 'live-person',
			source: 'camera',
			kind: 'person',
			start_time_ms: Date.now() - 1_000,
			end_time_ms: null,
			confidence: 0.93,
			bbox: null,
			zone: 'porch',
			thumbnail_url: null
		} satisfies RecordingEvent
	});
	await page.clock.fastForward(5_000);
	await expect(page.locator('[data-event-card="front-door:live-person"]')).toBeVisible({
		timeout: 2_000
	});
	expect(requests.storedTimelineQueries.length).toBeGreaterThan(initialQueryCount);
	const refresh = requests.storedTimelineQueries.at(-1);
	expect(refresh?.includeAttachments).toBe(false);
	expect(refresh?.includeAvailability).toBe(false);
	expect((refresh?.endMs ?? 0) - (refresh?.startMs ?? 0)).toBeLessThanOrEqual(5 * 60_000);
});

test('ends a failed preview cleanly and retries it from event detail', async ({ page }) => {
	await mockEventsWithUnavailablePreview(page);
	await page.goto(`/events?date=${eventDate}`);

	const card = page.locator('[data-event-card="front-door:person-high"]');
	await expect(card.getByLabel('Event image unavailable')).toBeVisible();
	await card.click();
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	await expect(detail.getByText('PREVIEW UNAVAILABLE', { exact: true })).toBeVisible();
	await detail.getByRole('button', { name: 'Retry preview' }).click();
	await expect(detail.getByText('PREVIEW UNAVAILABLE', { exact: true })).toBeVisible();
});

test('Board 10 detail restores its deep link and exposes only returned Event evidence', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	await mockEvents(page);
	await page.goto(`/events?date=${eventDate}&event=person-high&eventCamera=front-door`);

	await expect(page).toHaveTitle('Events - KeepPeek');
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	await expect(detail).toBeVisible();
	await expect(page).toHaveURL(/event=person-high&eventCamera=front-door/);
	await expect(detail.getByText('porch', { exact: true })).toBeVisible();
	await expect(detail.getByText('0.300, 0.200, 0.250, 0.500', { exact: true })).toBeVisible();
	await expect(detail.getByText('Camera event source', { exact: true })).toBeVisible();
	await expect(detail.getByText('REVISION 1', { exact: true })).toBeVisible();
	await expect(detail.getByText('front-door', { exact: true })).toBeVisible();
	await expect(detail.getByText('Not reported by REST API', { exact: true })).toHaveCount(1);
	await expect(
		detail
			.locator('[data-capability-gate][data-capability="keeppeek.media-export.v1"]')
			.filter({ hasText: 'Export this moment' })
	).toBeVisible();
	await expect(
		detail
			.locator('[data-capability-gate][data-capability="keeppeek.bookmarks.v1"]')
			.filter({ hasText: 'Bookmark' })
	).toBeVisible();
	await expect(detail.getByRole('link', { name: 'Open at this moment' })).toHaveAttribute(
		'href',
		/\/keep\?camera=front-door&stream=main&date=2026-08-18&at=\d+/
	);
	await page.reload();
	await expect(page.getByRole('complementary', { name: 'Event detail' })).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(page).not.toHaveURL(/event=/);
	await page.locator('[data-event-card="front-door:person-high"]').click();
	const expectedTimestampMs = Date.parse('2026-08-18T06:37:23Z');
	await page
		.getByRole('complementary', { name: 'Event detail' })
		.getByRole('link', { name: 'Open at this moment' })
		.click();
	await expect(page).toHaveURL(/\/keep\?camera=front-door&stream=main&date=2026-08-18/);
	await expect(page.getByRole('region', { name: 'Recorded video player' })).toHaveAttribute(
		'data-recording-playhead-ms',
		String(expectedTimestampMs)
	);
});

test('keeps mixed Event cards and detail usable at the authored mobile viewport', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockEvents(page);
	await page.goto(`/events?date=${eventDate}`);

	await expect(page.locator('[data-event-card]')).toHaveCount(5);
	await expect(page.getByLabel('Event filters')).toBeVisible();
	await page.locator('[data-event-card="front-door:motion-no-image"]').click();
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	await expect(detail).toBeVisible();
	await expect(detail.getByText('NO IMAGE REPORTED', { exact: true })).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	await page.getByRole('button', { name: 'Close event detail', exact: true }).click();
	await expect(page.locator('[data-event-card]')).toHaveCount(5);
});
