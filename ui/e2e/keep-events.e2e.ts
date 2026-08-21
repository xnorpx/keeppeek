import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const jpeg = Buffer.from(
	'/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////2wBDAf//////////////////////////////////////////////////////////////////////////////////////wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAX/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIQAxAAAAEf/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPwF//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPyF//9oADAMBAAIAAwAAABD/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oACAEDAQE/EP/EABQRAQAAAAAAAAAAAAAAAAAAABD/2gAIAQIBAT8Q/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxB//9k=',
	'base64'
);

test('renders camera motion thumbnails in the recording timeline', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const cameras = [
		{
			id: 'camera-1',
			ip: '192.0.2.1',
			name: 'Front Door',
			manufacturer: 'Reolink',
			model: 'RLC-820A',
			firmware_version: null,
			is_reolink: true,
			profiles: []
		}
	];
	await page.addInitScript(() => {
		Object.defineProperty(HTMLMediaElement.prototype, 'play', {
			configurable: true,
			value() {
				this.dataset.playRequested = 'true';
				return Promise.resolve();
			}
		});
	});
	await mockControlPeer(page, {
		cameras,
		storedRanges: [
			{
				sourceId: 'camera-1',
				streamId: 'main',
				startMs: Date.UTC(2026, 7, 10),
				endMs: Date.UTC(2026, 7, 10, 0, 10)
			}
		],
		storedEvents: [
			{
				sourceId: 'camera-1',
				thumbnail: jpeg,
				event: {
					id: 'event-1',
					source: 'camera',
					kind: 'motion',
					start_time_ms: Date.UTC(2026, 7, 10, 0, 5),
					end_time_ms: Date.UTC(2026, 7, 10, 0, 5, 15),
					confidence: null,
					bbox: null,
					zone: null,
					thumbnail_url: null
				}
			}
		]
	});
	await page.goto('/keep?camera=camera-1&stream=main&date=2026-08-10');

	const event = page.getByRole('button', { name: 'Motion event at 00:05' });
	await expect(event).toBeVisible();
	await expect(event.locator('img')).toHaveAttribute('loading', 'lazy');
	await expect(event.locator('img')).toHaveAttribute('decoding', 'async');
	await expect(
		page.getByRole('region', { name: 'Recorded video player' }).getByText('Front Door')
	).toBeVisible();
	await event.click();
	await expect(page.locator('video')).toHaveAttribute('data-play-requested', 'true');
	expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);
});
