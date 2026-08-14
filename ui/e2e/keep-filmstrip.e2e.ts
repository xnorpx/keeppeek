import { expect, test } from '@playwright/test';

const day = '2026-08-10';
const dayStartMs = Date.UTC(2026, 7, 10);

function camera(id: string, name: string) {
	return {
		id,
		ip: id === 'deck' ? '192.0.2.10' : '192.0.2.11',
		name,
		manufacturer: 'Test',
		model: 'Fixture',
		firmware_version: null,
		is_reolink: false,
		profiles: [
			{ name: 'Main', stream: 'main', encoding: 'h265', resolution: '1920x1080', framerate: 20 },
			{ name: 'Sub', stream: 'sub', encoding: 'h264', resolution: '640x360', framerate: 15 }
		]
	};
}

test('switches synchronized historical cameras from the Keep filmstrip', async ({ page }) => {
	await page.addInitScript(() => {
		class VisibleIntersectionObserver {
			constructor(private callback: IntersectionObserverCallback) {}

			observe(target: Element) {
				this.callback(
					[{ isIntersecting: true, intersectionRatio: 1, target } as IntersectionObserverEntry],
					this as unknown as IntersectionObserver
				);
			}

			unobserve() {}
			disconnect() {}
			takeRecords(): IntersectionObserverEntry[] {
				return [];
			}
		}
		Object.defineProperty(window, 'IntersectionObserver', {
			configurable: true,
			value: VisibleIntersectionObserver
		});
		const currentTimes = new WeakMap<HTMLMediaElement, number>();
		const playing = new WeakSet<HTMLMediaElement>();
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get() {
				return currentTimes.get(this) ?? 0;
			},
			set(value: number) {
				currentTimes.set(this, value);
			}
		});
		Object.defineProperty(HTMLMediaElement.prototype, 'readyState', {
			configurable: true,
			get: () => 1
		});
		Object.defineProperty(HTMLMediaElement.prototype, 'paused', {
			configurable: true,
			get() {
				return !playing.has(this);
			}
		});
		Object.defineProperty(HTMLMediaElement.prototype, 'play', {
			configurable: true,
			value() {
				playing.add(this);
				this.dataset.playRequested = 'true';
				return Promise.resolve();
			}
		});
		Object.defineProperty(HTMLMediaElement.prototype, 'pause', {
			configurable: true,
			value() {
				playing.delete(this);
				this.dataset.pauseRequested = 'true';
			}
		});
		Object.defineProperty(HTMLMediaElement.prototype, 'load', {
			configurable: true,
			value() {}
		});
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({
			json: [camera('deck', 'Deck'), camera('garden', 'Garden'), camera('gate', 'Gate')]
		});
	});
	await page.route('**/api/recordings/*', async (route) => {
		const cameraId = new URL(route.request().url()).pathname.split('/').at(-1)!;
		await route.fulfill({
			json: {
				camera_id: cameraId,
				date: day,
				dates: [day],
				segments: [
					{
						stream: 'main',
						date: day,
						hour: '10',
						filename: `${cameraId}-main.mp4`,
						url: `/${cameraId}-main.mp4`,
						start_time_ms: dayStartMs + 10 * 60 * 60_000,
						end_time_ms: dayStartMs + 11 * 60 * 60_000,
						duration_ms: 60 * 60_000
					},
					{
						stream: 'sub',
						date: day,
						hour: '10',
						filename: `${cameraId}-sub.mp4`,
						url: `/${cameraId}-sub.mp4`,
						start_time_ms: dayStartMs + 10 * 60 * 60_000,
						end_time_ms: dayStartMs + 11 * 60 * 60_000,
						duration_ms: 60 * 60_000
					}
				]
			}
		});
	});
	await page.route('**/api/events/*', async (route) => {
		const cameraId = new URL(route.request().url()).pathname.split('/').at(-1)!;
		await route.fulfill({ json: { camera_id: cameraId, date: day, events: [] } });
	});
	await page.route('**/api/recordings/*/*/activity', async (route) => {
		await route.fulfill({ status: 204 });
	});
	await page.route('**/*.mp4', async (route) => {
		await route.fulfill({ status: 200, contentType: 'video/mp4', body: Buffer.alloc(0) });
	});

	await page.goto(`/keep?camera=deck&stream=main&date=${day}`);

	const player = page.getByRole('region', { name: 'Recorded video player' });
	await expect(player.getByText('Deck', { exact: true })).toBeVisible();
	const filmstrip = page.getByRole('region', { name: 'Other camera recordings' });
	await expect(filmstrip).toBeVisible();
	await filmstrip.scrollIntoViewIfNeeded();
	await expect(filmstrip.getByRole('button', { name: /Review Deck at/ })).toHaveCount(0);
	const garden = filmstrip.getByRole('button', { name: 'Review Garden at 10:00 UTC' });
	await expect(garden).toBeVisible();
	await expect(filmstrip.getByRole('button', { name: 'Review Gate at 10:00 UTC' })).toBeVisible();

	const mainVideo = player.locator('video:not([data-recording-preview])');
	const visiblePreviews = filmstrip.locator('video[data-recording-preview]');
	await expect(visiblePreviews).toHaveCount(2);
	await expect(visiblePreviews.first()).toHaveAttribute('preload', 'metadata');
	await expect(visiblePreviews.last()).toHaveAttribute('preload', 'metadata');
	await expect(mainVideo).toHaveAttribute('src', '/deck-main.mp4');
	await mainVideo.evaluate(async (node) => {
		node.currentTime = 120;
		await node.play();
		node.dispatchEvent(new Event('play'));
		node.dispatchEvent(new Event('timeupdate'));
	});
	await expect
		.poll(() => visiblePreviews.evaluateAll((videos) => videos.map((video) => video.currentTime)))
		.toEqual([120, 120]);
	await expect(visiblePreviews.first()).toHaveAttribute('data-play-requested', 'true');
	await expect(visiblePreviews.last()).toHaveAttribute('data-play-requested', 'true');

	await mainVideo.evaluate((node) => {
		node.pause();
		node.dispatchEvent(new Event('pause'));
	});
	await expect(visiblePreviews.first()).toHaveAttribute('data-pause-requested', 'true');
	await expect(visiblePreviews.last()).toHaveAttribute('data-pause-requested', 'true');
	await filmstrip.getByRole('button', { name: /Review Garden at/ }).click();

	await expect(page).toHaveURL(new RegExp(`/keep\\?camera=garden&stream=main&date=${day}`));
	await expect(player.getByText('Garden', { exact: true })).toBeVisible();
	await expect(filmstrip.getByRole('button', { name: /Review Garden at/ })).toHaveCount(0);
	await expect(filmstrip.getByRole('button', { name: 'Review Deck at 10:02 UTC' })).toBeVisible();
	await expect(filmstrip.getByRole('button', { name: 'Review Gate at 10:02 UTC' })).toBeVisible();
});
