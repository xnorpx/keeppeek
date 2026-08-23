import { expect, test } from '@playwright/test';
import type { CameraListItem } from '../src/lib/types';
import { mockControlPeer } from './fixtures/control-peer';

const day = '2026-08-10';
const dayStartMs = Date.UTC(2026, 7, 10);

function camera(id: string, name: string): CameraListItem {
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
	const cameras = [camera('deck', 'Deck'), camera('garden', 'Garden'), camera('gate', 'Gate')];
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
	const requests = await mockControlPeer(page, {
		cameras,
		storedRanges: ['deck', 'garden', 'gate'].flatMap((sourceId) =>
			(['main', 'sub'] as const).map((streamId) => ({
				sourceId,
				streamId,
				startMs: dayStartMs + 10 * 60 * 60_000,
				endMs: dayStartMs + 11 * 60 * 60_000
			}))
		)
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
	await expect(mainVideo).toHaveAttribute('src', /^blob:/);
	await expect(visiblePreviews.first()).toHaveAttribute('src', /^blob:/);
	await expect(visiblePreviews.last()).toHaveAttribute('src', /^blob:/);
	expect(requests.storedOpens.filter((request) => request.sourceId === 'garden')).toHaveLength(1);
	expect(requests.storedOpens.filter((request) => request.sourceId === 'gate')).toHaveLength(1);
	await mainVideo.evaluate(async (node) => {
		const video = node as HTMLVideoElement;
		video.currentTime = 120;
		await video.play();
		video.dispatchEvent(new Event('play'));
		video.dispatchEvent(new Event('timeupdate'));
	});
	await expect
		.poll(() =>
			visiblePreviews.evaluateAll((videos) =>
				videos.map(
					(video) =>
						Number((video as HTMLElement).dataset.playbackAnchorMs) +
						(video as HTMLVideoElement).currentTime * 1_000
				)
			)
		)
		.toEqual([dayStartMs + 10 * 60 * 60_000 + 120_000, dayStartMs + 10 * 60 * 60_000 + 120_000]);
	await expect(visiblePreviews.first()).toHaveAttribute('data-play-requested', 'true');
	await expect(visiblePreviews.last()).toHaveAttribute('data-play-requested', 'true');
	const synchronizedGarden = filmstrip.getByRole('button', { name: 'Review Garden at 10:02 UTC' });
	await expect(synchronizedGarden).toBeVisible();

	await mainVideo.evaluate((node) => {
		const video = node as HTMLVideoElement;
		video.pause();
		video.dispatchEvent(new Event('pause'));
	});
	await expect(visiblePreviews.first()).toHaveAttribute('data-pause-requested', 'true');
	await expect(visiblePreviews.last()).toHaveAttribute('data-pause-requested', 'true');
	expect(requests.storedOpens.filter((request) => request.sourceId === 'garden')).toHaveLength(1);
	expect(requests.storedOpens.filter((request) => request.sourceId === 'gate')).toHaveLength(1);
	await expect(synchronizedGarden).toBeVisible();
	await synchronizedGarden.click();

	await expect(page).toHaveURL(new RegExp(`/keep\\?camera=garden&stream=main&date=${day}`));
	await expect(player.getByText('Garden', { exact: true })).toBeVisible();
	await expect(filmstrip.getByRole('button', { name: /Review Garden at/ })).toHaveCount(0);
	await expect(filmstrip.getByRole('button', { name: 'Review Deck at 10:02 UTC' })).toBeVisible();
	await expect(filmstrip.getByRole('button', { name: 'Review Gate at 10:02 UTC' })).toBeVisible();
});
