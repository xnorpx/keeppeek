import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const date = '2026-08-10';
const dayStartMs = Date.parse(`${date}T00:00:00Z`);
const newestMs = dayStartMs + 6 * 60 * 60_000 + 40 * 60_000;
const jpeg = Buffer.from(
	'/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////2wBDAf//////////////////////////////////////////////////////////////////////////////////////wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAX/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIQAxAAAAEf/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPwF//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPyF//9oADAMBAAIAAwAAABD/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oACAEDAQE/EP/EABQRAQAAAAAAAAAAAAAAAAAAABD/2gAIAQIBAT8Q/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxB//9k=',
	'base64'
);

async function mockKeepTimeline(
	page: Page,
	options: { storedOpenGates?: readonly Promise<void>[] } = {}
): Promise<void> {
	const cameras = [
		{
			id: 'front-door',
			ip: '192.0.2.1',
			name: 'Front Door',
			manufacturer: 'Reolink',
			model: 'RLC-811A',
			firmware_version: null,
			is_reolink: true,
			profiles: []
		}
	];
	await page.addInitScript((frozenNowMs) => {
		Date.now = () => frozenNowMs;
		Object.defineProperty(HTMLMediaElement.prototype, 'play', {
			configurable: true,
			value() {
				this.dataset.playRequested = 'true';
				return Promise.resolve();
			}
		});
	}, newestMs);
	await mockControlPeer(page, {
		cameras,
		storedOpenGates: options.storedOpenGates,
		storedRanges: [
			{
				sourceId: 'front-door',
				streamId: 'main',
				startMs: dayStartMs + 6 * 60 * 60_000,
				endMs: dayStartMs + 6 * 60 * 60_000 + 10 * 60_000
			},
			{
				sourceId: 'front-door',
				streamId: 'main',
				startMs: dayStartMs + 6 * 60 * 60_000 + 15 * 60_000,
				endMs: newestMs
			}
		],
		storedEvents: [
			{
				sourceId: 'front-door',
				thumbnail: jpeg,
				event: {
					id: 'event-1',
					source: 'camera',
					kind: 'person',
					start_time_ms: newestMs - 60_000,
					end_time_ms: newestMs - 55_000,
					confidence: 0.91,
					bbox: null,
					zone: null,
					thumbnail_url: null
				}
			},
			{
				sourceId: 'front-door',
				event: {
					id: 'event-2',
					source: 'keeppeek',
					kind: 'motion',
					start_time_ms: newestMs - 70_000,
					end_time_ms: null,
					confidence: null,
					bbox: null,
					zone: null,
					thumbnail_url: null
				}
			}
		]
	});
}

test('Board 4 renders the newest-at-top timeline with explicit gaps and live follow', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);

	const timeline = page.getByRole('region', { name: 'Recording timeline', exact: true });
	await expect(timeline).toHaveAttribute('data-timeline-zoom', '6h');
	await expect(timeline).toHaveAttribute('data-timeline-following', 'true');
	await expect(timeline.getByText('LIVE', { exact: true })).toBeVisible();
	await expect(timeline.locator('[data-timeline-availability]')).toHaveCount(2);
	const gapStartMs = dayStartMs + 6 * 60 * 60_000 + 10 * 60_000;
	const gapEndMs = dayStartMs + 6 * 60 * 60_000 + 15 * 60_000;
	const gap = timeline.locator(
		`[data-timeline-gap][data-start-ms="${gapStartMs}"][data-end-ms="${gapEndMs}"]`
	);
	await expect(gap).toBeVisible();
	const newestRange = timeline.locator(`[data-timeline-availability][data-end-ms="${newestMs}"]`);
	const olderRange = timeline.locator(
		`[data-timeline-availability][data-start-ms="${dayStartMs + 6 * 60 * 60_000}"]`
	);
	await expect
		.poll(async () => {
			const [newestBounds, olderBounds] = await Promise.all([
				newestRange.boundingBox(),
				olderRange.boundingBox()
			]);
			return newestBounds !== null && olderBounds !== null && newestBounds.y < olderBounds.y;
		})
		.toBe(true);
	await expect(timeline.getByRole('button', { name: /person event/i })).toContainText('+1');
	await timeline.getByRole('button', { name: 'Motion', exact: true }).click();
	await expect(timeline.getByRole('button', { name: /motion event/i })).toBeVisible();
	await expect(timeline.getByRole('button', { name: /person event/i })).toHaveCount(0);
	await timeline.getByRole('button', { name: 'All', exact: true }).click();
	await expect(timeline.getByRole('button', { name: /person event/i })).toContainText('+1');

	await timeline.getByTitle('Zoom timeline in').click();
	await expect(timeline).toHaveAttribute('data-timeline-zoom', '1h');
	const viewport = page.getByRole('region', { name: 'Recording timeline pan viewport' });
	await viewport.dispatchEvent('wheel', { deltaY: 100 });
	await expect(timeline).toHaveAttribute('data-timeline-following', 'false');
	await expect(timeline.getByRole('button', { name: 'Back to live' })).toBeVisible();
	await timeline.getByRole('button', { name: 'Back to live' }).click();
	await expect(timeline).toHaveAttribute('data-timeline-following', 'true');
	await expect(page.locator('video')).toHaveAttribute('data-play-requested', 'true');
});

test('contains the Paper timeline lanes at the authored mobile viewport', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);

	await expect(page.getByRole('region', { name: 'Recording timeline', exact: true })).toBeVisible();
	await expect(page.locator('[data-timeline-gap]')).toHaveCount(2);
	await expect
		.poll(async () => {
			const [timelineBounds, cardBounds] = await Promise.all([
				page.getByRole('region', { name: 'Recording timeline', exact: true }).boundingBox(),
				page.getByRole('button', { name: /person event/i }).boundingBox()
			]);
			return (
				timelineBounds !== null &&
				cardBounds !== null &&
				cardBounds.x >= timelineBounds.x &&
				cardBounds.x + cardBounds.width <= timelineBounds.x + timelineBounds.width
			);
		})
		.toBe(true);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('names a cold seek after 400ms while preserving the current frame', async ({ page }) => {
	let releaseColdSeek!: () => void;
	const coldSeekGate = new Promise<void>((resolve) => {
		releaseColdSeek = resolve;
	});
	await mockKeepTimeline(page, { storedOpenGates: [Promise.resolve(), coldSeekGate] });
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);

	const video = page.locator('video');
	await expect(video).toBeVisible();
	const currentSource = await video.getAttribute('src');
	const olderRange = page.locator(
		`[data-timeline-availability][data-start-ms="${dayStartMs + 6 * 60 * 60_000}"]`
	);
	await olderRange.click();
	await expect(page.locator('[data-cold-seek]')).toHaveCount(0);
	await expect(page.locator('[data-cold-seek]')).toBeVisible();
	await expect(page.locator('[data-cold-seek]')).toContainText('Opening indexed recording');
	await expect(page.locator('[data-cold-seek]')).toContainText(
		'The current frame stays until the requested recording arrives'
	);
	await expect(video).toHaveAttribute('src', currentSource!);

	releaseColdSeek();
	await expect(page.locator('[data-cold-seek]')).toHaveCount(0);
	await expect.poll(() => video.getAttribute('src')).not.toBe(currentSource);
});
