import { expect, test } from '@playwright/test';
import type { CameraListItem, RecordingEvent } from '../src/lib/types';
import { mockControlPeer, type StoredEventFixture } from './fixtures/control-peer';
import {
	eventDate,
	mockDenseEvents,
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

test('sends UTC range and zone filters server-side with bounded text debounce', async ({
	page
}) => {
	const requests = await mockEvents(page);
	await page.goto(`/events?date=${eventDate}`);
	await expect(page.locator('[data-event-card]')).toHaveCount(5);

	await page.getByLabel('Start time UTC').fill('06:00');
	await page.getByLabel('Start time UTC').press('Tab');
	await page.getByLabel('End time UTC').fill('07:00');
	await page.getByLabel('End time UTC').press('Tab');
	await page.getByLabel('Zone').fill('porch');
	await page.getByRole('searchbox', { name: 'Search events' }).fill('p');
	await page.getByRole('searchbox', { name: 'Search events' }).fill('pe');
	await page.getByRole('searchbox', { name: 'Search events' }).fill('person');

	await expect(page.locator('[data-event-card="front-door:person-high"]')).toBeVisible();
	await expect(page.locator('[data-event-card]')).toHaveCount(1);
	const query = requests.eventSearchQueries
		.filter((request) => !request.includePreviewKeyframes)
		.at(-1);
	expect(query?.startMs).toBe(Date.parse(`${eventDate}T06:00:00Z`));
	expect(query?.endMs).toBe(Date.parse(`${eventDate}T07:00:00Z`));
	expect(query?.zones).toEqual(['porch']);
	expect(query?.text).toBe('person');
	expect(query?.pageSize).toBe(18);
	await expect(page).toHaveURL(/from=06%3A00&to=07%3A00.*zone=porch.*q=person/);
});

test('bounds lazy preview concurrency and cancels media on route exit', async ({ page }) => {
	const releases: Array<() => void> = [];
	const gates = Array.from(
		{ length: 4 },
		() =>
			new Promise<void>((resolve) => {
				releases.push(resolve);
			})
	);
	const requests = await mockEvents(page, gates);
	await page.goto(`/events?date=${eventDate}`);

	await expect.poll(() => requests.eventMediaFetches.length).toBe(2);
	expect(requests.eventSearchQueries[0]?.includePreviewKeyframes).toBe(false);
	const previewQueries = requests.eventSearchQueries.filter(
		(query) => query.includePreviewKeyframes
	);
	expect(previewQueries).toHaveLength(0);
	expect(requests.maxConcurrentEventMedia).toBe(2);
	releases.shift()?.();
	await expect.poll(() => requests.eventMediaFetches.length).toBe(3);
	expect(requests.maxConcurrentEventMedia).toBe(2);

	await page.getByRole('link', { name: 'Cameras', exact: true }).click();
	await expect.poll(() => requests.cancelledEventMedia.length).toBeGreaterThan(0);
	for (const release of releases) release();
});

test('meets dense metadata-first DOM, transfer, and long-task budgets', async ({
	page
}, testInfo) => {
	await page.addInitScript(() => {
		const state = { activeObjectUrls: new Set<string>(), longTasks: [] as number[] };
		(window as unknown as { __eventPerformance: typeof state }).__eventPerformance = state;
		const createObjectUrl = URL.createObjectURL.bind(URL);
		const revokeObjectUrl = URL.revokeObjectURL.bind(URL);
		URL.createObjectURL = (object) => {
			const url = createObjectUrl(object);
			state.activeObjectUrls.add(url);
			return url;
		};
		URL.revokeObjectURL = (url) => {
			state.activeObjectUrls.delete(url);
			revokeObjectUrl(url);
		};
		new PerformanceObserver((list) => {
			state.longTasks.push(...list.getEntries().map((entry) => entry.duration));
		}).observe({ type: 'longtask', buffered: true });
	});
	const requests = await mockDenseEvents(page);
	const startedAt = performance.now();
	await page.goto(`/events?date=${eventDate}`);
	await expect(page.locator('[data-event-card]')).toHaveCount(18);
	const firstPageMs = performance.now() - startedAt;

	expect(requests.eventSearchQueries).toHaveLength(1);
	expect(requests.eventSearchQueries[0]?.pageSize).toBe(18);
	expect(requests.storedTimelineQueries).toHaveLength(0);
	expect(requests.eventMediaFetches).toHaveLength(0);
	await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
	await expect(page.locator('[data-event-card]')).toHaveCount(18);
	const metrics = await page.evaluate(() => {
		const state = (
			window as unknown as {
				__eventPerformance: { activeObjectUrls: Set<string>; longTasks: number[] };
			}
		).__eventPerformance;
		return {
			activeObjectUrls: state.activeObjectUrls.size,
			maxLongTaskMs: Math.max(0, ...state.longTasks),
			eventCards: document.querySelectorAll('[data-event-card]').length
		};
	});
	await testInfo.attach('event-performance.json', {
		body: JSON.stringify({ firstPageMs, ...metrics }, null, 2),
		contentType: 'application/json'
	});
	const contendedRunner = Boolean(process.env.CI) || testInfo.config.workers > 1;
	const firstPageBudgetMs = contendedRunner ? 2_000 : 1_000;
	const longTaskBudgetMs = contendedRunner ? 150 : 50;
	expect(firstPageMs).toBeLessThan(firstPageBudgetMs);
	expect(metrics.eventCards).toBe(18);
	expect(metrics.activeObjectUrls).toBe(0);
	expect(metrics.maxLongTaskMs).toBeLessThanOrEqual(longTaskBudgetMs);
});

test('continues Events with an opaque server page token', async ({ page }) => {
	await mockEventsWithOlderFilteredMatch(page);
	await page.goto(`/events?date=${eventDate}`);

	await expect(page.locator('[data-event-card]')).toHaveCount(18);
	await expect(page.getByText('1-18+', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Earlier events' }).click();

	await expect(page.locator('[data-event-card="front-door:older-person"]')).toBeVisible();
	await expect(page.getByText('19-19', { exact: true })).toBeVisible();
	await page.locator('[data-event-card="front-door:older-person"]').click();
	await page
		.getByRole('complementary', { name: 'Event detail' })
		.getByRole('link', { name: 'Open at this moment' })
		.click();
	await expect(page).toHaveURL(/\/keep\?/);
	await page.goBack();
	await expect(page).toHaveURL(/event=older-person&eventCamera=front-door/);
	await expect(page.getByRole('complementary', { name: 'Event detail' })).toBeVisible();
	await expect(page.getByText('19-19', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Close event detail', exact: true }).click();
	await page.getByRole('button', { name: 'Newer events' }).click();
	await expect(page.locator('[data-event-card]')).toHaveCount(18);
});

test('recovers an expired continuation token from the newest page', async ({ page }) => {
	await mockEventsWithOlderFilteredMatch(
		page,
		[],
		'event search page token expired; restart the query'
	);
	await page.goto(`/events?date=${eventDate}`);
	await expect(page.locator('[data-event-card]')).toHaveCount(18);

	await page.getByRole('button', { name: 'Earlier events' }).click();

	await expect(
		page.getByText('Events changed while you were browsing. Refreshed from the newest page.')
	).toBeVisible();
	await expect(page.locator('[data-event-card]')).toHaveCount(18);
	await expect(page.getByText('1-18+', { exact: true })).toBeVisible();
});

test('resolves a selected event outside the visible page with one exact query', async ({
	page
}) => {
	const requests = await mockEventsWithOlderFilteredMatch(page);
	await page.goto(`/events?date=${eventDate}&event=older-person&eventCamera=front-door`);

	await expect(page.locator('[data-event-card]')).toHaveCount(18);
	await expect(page.getByRole('complementary', { name: 'Event detail' })).toBeVisible();
	await expect(page.getByText('19-19', { exact: true })).toHaveCount(0);
	const selectedQuery = requests.eventSearchQueries.find(
		(query) => query.eventIds[0] === 'older-person'
	);
	expect(selectedQuery?.eventIds).toEqual(['older-person']);
	expect(selectedQuery?.pageSize).toBe(1);
});

test('cancels an earlier Events search cleanly when the date changes', async ({ page }) => {
	let releaseEarlierSearch!: () => void;
	const earlierSearchGate = new Promise<void>((resolve) => {
		releaseEarlierSearch = resolve;
	});
	const requests = await mockEventsWithOlderFilteredMatch(page, [
		Promise.resolve(),
		earlierSearchGate
	]);
	await page.goto(`/events?date=${eventDate}`);

	await expect(page.locator('[data-event-card]')).toHaveCount(18);
	await page.getByRole('button', { name: 'Earlier events' }).click();
	await page.getByLabel('Event date').fill('2026-08-19');
	await page.getByLabel('Event date').press('Tab');
	releaseEarlierSearch();

	await expect(page).toHaveURL(/date=2026-08-19/);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByText('No events found.')).toBeVisible();
	await expect.poll(() => requests.cancelledEventSearchQueries.length).toBeGreaterThan(0);
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
	const initialQueryCount = requests.eventSearchQueries.length;
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
	expect(requests.eventSearchQueries.length).toBeGreaterThan(initialQueryCount);
	const refresh = requests.eventSearchQueries.at(-1);
	expect(refresh?.pageSize).toBe(18);
	expect(refresh?.pageToken).toBe('');
	expect(refresh?.sourceIds).toEqual(['front-door']);
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
		/\/keep\?camera=front-door&date=2026-08-18&at=\d+/
	);
	await page.reload();
	await expect(page.getByRole('complementary', { name: 'Event detail' })).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(page).not.toHaveURL(/event=/);
	await page.locator('[data-event-card="front-door:person-high"]').click();
	await page.goBack();
	await expect(page.getByRole('complementary', { name: 'Event detail' })).toHaveCount(0);
	await expect(page).toHaveURL(new RegExp(`/events\\?date=${eventDate}$`));
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
	await page.goBack();
	await expect(page.getByRole('complementary', { name: 'Event detail' })).toBeVisible();
	await expect(page).toHaveURL(/event=person-high&eventCamera=front-door/);
});

test('keeps mixed Event cards and detail usable at the authored mobile viewport', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockEvents(page);
	await page.goto(`/events?date=${eventDate}`);

	await expect(page.locator('[data-event-card]')).toHaveCount(5);
	await expect(page.getByLabel('Event filters')).toBeVisible();
	await expect(page.getByLabel('Start time UTC')).toBeHidden();
	const filterToggle = page.getByRole('button', { name: /Filters/ });
	await expect(filterToggle).toBeInViewport();
	await expect(page.locator('[data-event-card]').first()).toBeInViewport();
	await expect
		.poll(async () => {
			const bounds = await filterToggle.boundingBox();
			return bounds
				? [Math.round(bounds.width >= 44 ? 44 : bounds.width), Math.round(bounds.height)]
				: null;
		})
		.toEqual([44, 44]);
	await filterToggle.click();
	await expect(page.getByLabel('Start time UTC')).toBeVisible();
	await page.locator('[data-event-card="front-door:motion-no-image"]').click();
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	await expect(detail).toBeVisible();
	await expect(detail.getByText('NO IMAGE', { exact: true })).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	await page.getByRole('button', { name: 'Close event detail', exact: true }).click();
	await expect(page.locator('[data-event-card]')).toHaveCount(5);
});
