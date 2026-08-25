import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer, type ControlRequests } from './fixtures/control-peer';

const date = '2026-08-10';
const dayStartMs = Date.parse(`${date}T00:00:00Z`);
const newestMs = dayStartMs + 6 * 60 * 60_000 + 40 * 60_000;
const jpeg = Buffer.from(
	'/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////2wBDAf//////////////////////////////////////////////////////////////////////////////////////wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAX/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIQAxAAAAEf/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPwF//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPyF//9oADAMBAAIAAwAAABD/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oACAEDAQE/EP/EABQRAQAAAAAAAAAAAAAAAAAAABD/2gAIAQIBAT8Q/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxB//9k=',
	'base64'
);

async function mockKeepTimeline(
	page: Page,
	options: {
		storedOpenGates?: readonly Promise<void>[];
		storedBucketedRanges?: readonly {
			sourceId: string;
			streamId: 'main' | 'sub';
			startMs: number;
			endMs: number;
		}[];
		storedEvents?: NonNullable<Parameters<typeof mockControlPeer>[1]>['storedEvents'];
		storedSeekError?: string;
		mainEncoding?: string;
		emitLoadedData?: boolean;
	} = {}
): Promise<ControlRequests> {
	const cameras = [
		{
			id: 'front-door',
			ip: '192.0.2.1',
			name: 'Front Door',
			manufacturer: 'Reolink',
			model: 'RLC-811A',
			firmware_version: null,
			is_reolink: true,
			profiles: [
				{
					name: 'Main',
					stream: 'main' as const,
					encoding: options.mainEncoding ?? 'h264',
					resolution: '3840x2160',
					framerate: 25
				},
				{
					name: 'Sub',
					stream: 'sub' as const,
					encoding: 'h264',
					resolution: '640x360',
					framerate: 15
				}
			]
		}
	];
	await page.addInitScript(
		({ frozenNowMs, emitLoadedData }) => {
			Date.now = () => frozenNowMs;
			document.addEventListener(
				'error',
				(event) => {
					const target = event.target;
					if (
						target instanceof HTMLMediaElement &&
						target.dataset.keeppeekIntentionalError !== 'true'
					) {
						event.preventDefault();
						event.stopImmediatePropagation();
					}
				},
				true
			);
			Object.defineProperty(HTMLMediaElement.prototype, 'play', {
				configurable: true,
				value() {
					this.dataset.playRequested = 'true';
					if (emitLoadedData) {
						queueMicrotask(() => this.dispatchEvent(new Event('loadeddata')));
					}
					return Promise.resolve();
				}
			});
		},
		{ frozenNowMs: newestMs, emitLoadedData: options.emitLoadedData ?? true }
	);
	return mockControlPeer(page, {
		cameras,
		storedOpenGates: options.storedOpenGates,
		storedSeekError: options.storedSeekError,
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
			},
			{
				sourceId: 'front-door',
				streamId: 'sub',
				startMs: dayStartMs + 6 * 60 * 60_000,
				endMs: newestMs
			}
		],
		storedBucketedRanges: options.storedBucketedRanges,
		storedEvents: options.storedEvents ?? [
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

async function dispatchIntentionalMediaError(video: ReturnType<Page['locator']>): Promise<void> {
	await video.evaluate((element) => {
		const media = element as HTMLVideoElement;
		media.dataset.keeppeekIntentionalError = 'true';
		media.dispatchEvent(new Event('error'));
		delete media.dataset.keeppeekIntentionalError;
	});
}

test('defaults to the compatible H.264 main stream and autoplays when no stream is requested', async ({
	page
}) => {
	await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&date=${date}`);

	await expect(page).toHaveURL(new RegExp(`stream=main`));
	await expect(page.locator('video')).toHaveAttribute('data-play-requested', 'true');
});

test('explains a known codec fallback before opening the compatible substream', async ({
	page
}) => {
	const requests = await mockKeepTimeline(page, { mainEncoding: 'h265' });
	await page.goto(`/keep?camera=front-door&date=${date}`);

	await expect(page).toHaveURL(new RegExp(`stream=sub`));
	await expect(page.getByRole('status')).toContainText(
		'Main uses h265, which this browser cannot decode. Playing Sub instead.'
	);
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-rejected-variants',
		'main:h265'
	);
	await expect.poll(() => requests.storedOpens.map((request) => request.streamId)).toEqual(['sub']);
});

test('persists a recorded quality preference and reapplies it without an explicit stream', async ({
	page
}) => {
	const requests = await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&date=${date}`);
	await expect
		.poll(() => requests.storedOpens.map((request) => request.streamId))
		.toEqual(['main']);

	await page.getByLabel('Quality').selectOption('low');
	await expect(page).toHaveURL(new RegExp('stream=sub'));
	await expect
		.poll(() => requests.storedOpens.map((request) => request.streamId))
		.toEqual(['main', 'sub']);
	await page.locator('video').evaluate((element) => {
		const video = element as HTMLVideoElement;
		video.muted = true;
		video.dispatchEvent(new Event('volumechange'));
		video.playbackRate = 2;
		video.dispatchEvent(new Event('ratechange'));
		video.dispatchEvent(new Event('pause'));
	});
	await expect
		.poll(() =>
			page.evaluate(() => {
				const value = localStorage.getItem('keeppeek-playback-preferences');
				return value ? JSON.parse(value) : null;
			})
		)
		.toMatchObject({
			version: 1,
			recorded: { cameras: { 'front-door': 'low' } },
			media: { muted: true, playbackRate: 2, playing: false }
		});

	await page.goto(`/keep?camera=front-door&date=${date}`);
	await expect(page.getByLabel('Quality')).toHaveValue('low');
	await expect(page).toHaveURL(new RegExp('stream=sub'));
	await expect.poll(() => requests.storedOpens.at(-1)?.streamId).toBe('sub');
	const reloadedVideo = page.locator('video');
	await expect(reloadedVideo).toHaveJSProperty('muted', true);
	await expect(reloadedVideo).toHaveJSProperty('playbackRate', 2);
	await expect(reloadedVideo).not.toHaveAttribute('data-play-requested', 'true');
});

test('tries one visible compatible fallback after startup failure', async ({ page }) => {
	const requests = await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&date=${date}`);

	await expect
		.poll(() => requests.storedOpens.map((request) => request.streamId))
		.toEqual(['main']);
	await dispatchIntentionalMediaError(page.locator('video'));
	await expect
		.poll(() => requests.storedOpens.map((request) => request.streamId))
		.toEqual(['main', 'sub']);
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-fallback-variant',
		'sub'
	);
	await expect(page.getByRole('status')).toContainText('Playing Sub instead.');

	await dispatchIntentionalMediaError(page.locator('video'));
	await expect(page.getByRole('alert')).toContainText('The compatible fallback also failed.');
	expect(requests.storedOpens.map((request) => request.streamId)).toEqual(['main', 'sub']);
});

test('starts one visible fallback within the bounded startup deadline', async ({ page }) => {
	const requests = await mockKeepTimeline(page, { emitLoadedData: false });
	await page.goto(`/keep?camera=front-door&date=${date}`);

	await expect
		.poll(() => requests.storedOpens.map((request) => request.streamId), { timeout: 5_000 })
		.toEqual(['main', 'sub']);
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-fallback-variant',
		'sub'
	);
	await expect(
		page.getByText('No recording initialization arrived within 3 seconds. Playing Sub instead.', {
			exact: true
		})
	).toBeVisible();
});

test('aborts and closes a stored open when the route changes', async ({ page }) => {
	let releaseOpen!: () => void;
	const openGate = new Promise<void>((resolve) => {
		releaseOpen = resolve;
	});
	const requests = await mockKeepTimeline(page, { storedOpenGates: [openGate] });
	await page.goto(`/keep?camera=front-door&date=${date}`);
	await expect.poll(() => requests.storedOpens.length).toBe(1);
	const storedMediaId = requests.storedOpens[0]!.storedMediaId;

	await page.getByRole('link', { name: 'Peek', exact: true }).click();
	releaseOpen();
	await expect.poll(() => requests.storedCloses).toContain(storedMediaId);
	await expect(page).toHaveURL(/\/$/);
});

test('loads timeline metadata as soon as the primary frame is ready', async ({ page }) => {
	await page.addInitScript(() => {
		const state = window as Window & { __keeppeekTimelineEvents?: string[] };
		state.__keeppeekTimelineEvents = [];
		window.addEventListener('keeppeek:timeline-performance', (event) => {
			state.__keeppeekTimelineEvents?.push((event as CustomEvent<{ name: string }>).detail.name);
		});
	});
	await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);

	const video = page.locator('video');
	await expect(video).toBeVisible();
	await video.dispatchEvent('loadeddata');
	await expect
		.poll(
			() =>
				page.evaluate(() =>
					(
						window as Window & { __keeppeekTimelineEvents?: string[] }
					).__keeppeekTimelineEvents?.includes('TimelineQueryStarted')
				),
			{ timeout: 1_000 }
		)
		.toBe(true);
});

test('uses exact recording ranges when a bucketed event falls inside a real gap', async ({
	page
}) => {
	const gapStartMs = dayStartMs + 6 * 60 * 60_000 + 10 * 60_000;
	const gapEndMs = dayStartMs + 6 * 60 * 60_000 + 15 * 60_000;
	const requests = await mockKeepTimeline(page, {
		storedBucketedRanges: [
			{
				sourceId: 'front-door',
				streamId: 'main',
				startMs: dayStartMs + 6 * 60 * 60_000,
				endMs: newestMs
			}
		],
		storedEvents: [
			{
				sourceId: 'front-door',
				event: {
					id: 'gap-person',
					source: 'camera',
					kind: 'person',
					start_time_ms: gapStartMs + 60_000,
					end_time_ms: null,
					confidence: 0.9,
					bbox: null,
					zone: null,
					thumbnail_url: null
				}
			}
		]
	});
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);
	await page.locator('video').dispatchEvent('loadeddata');

	await page.getByRole('button', { name: /person event/i }).click();
	await expect.poll(() => requests.storedSeeks.at(-1)?.timestampMs).toBe(gapEndMs);
	await expect(page.locator('[data-cold-seek]')).toHaveCount(0);
});

test('opens an event in a recording gap with one bounded exact-range query', async ({ page }) => {
	const gapTimestampMs = dayStartMs + 6 * 60 * 60_000 + 12 * 60_000;
	const nextRecordingMs = dayStartMs + 6 * 60 * 60_000 + 15 * 60_000;
	const requests = await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}&at=${gapTimestampMs}`);

	await expect(page.locator('video')).toBeVisible();
	expect(
		requests.storedTimelineQueries.filter(
			(query) =>
				query.includeAvailability &&
				query.startMs === gapTimestampMs - 5 * 60_000 &&
				query.endMs === gapTimestampMs + 5 * 60_000
		)
	).toHaveLength(1);
	expect(requests.storedOpens).toHaveLength(1);
	expect(requests.storedOpens[0]?.timestampMs).toBe(nextRecordingMs);
});

test('does not expand or open unrelated footage for an unavailable event timestamp', async ({
	page
}) => {
	const unavailableTimestampMs = dayStartMs + 2 * 60 * 60_000;
	const requests = await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}&at=${unavailableTimestampMs}`);

	await expect(page.getByText('No indexed footage is available near that time.')).toBeVisible();
	expect(
		requests.storedTimelineQueries.filter(
			(query) =>
				query.includeAvailability &&
				query.startMs === unavailableTimestampMs - 5 * 60_000 &&
				query.endMs === unavailableTimestampMs + 5 * 60_000
		)
	).toHaveLength(1);
	expect(requests.storedOpens).toHaveLength(0);
	await expect(page.locator('[data-cold-seek]')).toHaveCount(0);
});

test('clears the cold-seek overlay when an exact stored seek is rejected', async ({ page }) => {
	await mockKeepTimeline(page, { storedSeekError: 'stored media timestamp is unavailable' });
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);

	const newestRange = page.locator(`[data-timeline-availability][data-end-ms="${newestMs}"]`);
	await newestRange.click();
	await expect(page.getByText('No indexed footage is available at that exact time.')).toBeVisible();
	await expect(page.locator('[data-cold-seek]')).toHaveCount(0);
});

test('coalesces rapid timeline drag samples onto one stored-media cursor', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	const requests = await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);

	const playhead = page.getByRole('button', { name: /Playback position at/i });
	const bounds = await playhead.boundingBox();
	if (!bounds) throw new Error('Timeline playhead has no bounds');
	const pointerX = bounds.x + bounds.width / 2;
	const pointerY = bounds.y + bounds.height / 2;
	const dragStartedAt = performance.now();
	await page.mouse.move(pointerX, pointerY);
	await page.mouse.down();
	await page.mouse.move(pointerX, pointerY + 120, { steps: 30 });
	await page.mouse.up();
	const dragElapsedMs = performance.now() - dragStartedAt;

	await expect.poll(() => requests.storedSeeks.length).toBeGreaterThan(0);
	const cursorIds = new Set([
		...requests.storedOpens.map((request) => request.storedMediaId),
		...requests.storedSeeks.map((request) => request.storedMediaId)
	]);
	expect(cursorIds.size).toBe(1);
	expect(requests.storedOpens).toHaveLength(1);
	expect(requests.storedSeeks.length).toBeLessThanOrEqual(Math.ceil(dragElapsedMs / 50) + 1);
});

test('Board 4 renders the newest-at-top timeline with explicit gaps and live follow', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	const requests = await mockKeepTimeline(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);

	const timeline = page.getByRole('region', { name: 'Recording timeline', exact: true });
	await expect(timeline).toHaveAttribute('data-timeline-zoom', '6h');
	await expect(timeline).toHaveAttribute('data-timeline-following', 'true');
	await expect(page.locator('video')).toHaveAttribute('data-play-requested', 'true');
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
	const viewport = page.getByRole('region', { name: 'Recording timeline scroll viewport' });
	const scrollControl = timeline.getByRole('button', { name: /scroll recording timeline/i });
	await expect(viewport).toHaveCSS('cursor', 'default');
	const viewportBounds = await viewport.boundingBox();
	if (!viewportBounds) throw new Error('Recording timeline viewport has no bounds');
	const seekCount = requests.storedSeeks.length;
	const initialScrollTop = await viewport.evaluate((element) => element.scrollTop);
	await page.mouse.move(
		viewportBounds.x + viewportBounds.width / 2,
		viewportBounds.y + viewportBounds.height / 2
	);
	await page.mouse.wheel(0, 180);
	await expect
		.poll(() => viewport.evaluate((element) => element.scrollTop))
		.toBeGreaterThan(initialScrollTop);
	await expect(scrollControl).toBeFocused();
	await expect(timeline).toHaveAttribute('data-timeline-following', 'false');
	const wheelScrollTop = await viewport.evaluate((element) => element.scrollTop);
	await page.keyboard.press('ArrowDown');
	await expect
		.poll(() => viewport.evaluate((element) => element.scrollTop))
		.toBeGreaterThan(wheelScrollTop);
	expect(requests.storedSeeks).toHaveLength(seekCount);
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
	await expect.poll(() => video.getAttribute('src')).not.toBe(currentSource);
	await video.dispatchEvent('loadeddata');
	await expect(page.locator('[data-cold-seek]')).toHaveCount(0);
});

test('falls back once when the media element rejects playback during a cold seek', async ({
	page
}) => {
	let releaseColdSeek!: () => void;
	const coldSeekGate = new Promise<void>((resolve) => {
		releaseColdSeek = resolve;
	});
	await mockKeepTimeline(page, { storedOpenGates: [Promise.resolve(), coldSeekGate] });
	await page.goto(`/keep?camera=front-door&stream=main&date=${date}`);

	const video = page.locator('video');
	const currentSource = await video.getAttribute('src');
	const olderRange = page.locator(
		`[data-timeline-availability][data-start-ms="${dayStartMs + 6 * 60 * 60_000}"]`
	);
	await olderRange.click();
	await expect(page.locator('[data-cold-seek]')).toBeVisible();

	await dispatchIntentionalMediaError(video);
	releaseColdSeek();
	await expect(page.locator('[data-cold-seek]')).toHaveCount(0);
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-fallback-variant',
		'sub'
	);
	await expect(page.getByRole('status')).toContainText('Playing Sub instead.');
	await expect.poll(() => video.getAttribute('src')).not.toBe(currentSource);
});
