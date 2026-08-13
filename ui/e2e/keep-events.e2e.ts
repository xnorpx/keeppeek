import { expect, test } from '@playwright/test';

const jpeg = Buffer.from(
	'/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////2wBDAf//////////////////////////////////////////////////////////////////////////////////////wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAX/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIQAxAAAAEf/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPwF//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPyF//9oADAMBAAIAAwAAABD/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oACAEDAQE/EP/EABQRAQAAAAAAAAAAAAAAAAAAABD/2gAIAQIBAT8Q/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxB//9k=',
	'base64'
);

test('renders camera motion thumbnails in the recording timeline', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await page.addInitScript(() => {
		Object.defineProperty(HTMLMediaElement.prototype, 'play', {
			configurable: true,
			value() {
				this.dataset.playRequested = 'true';
				return Promise.resolve();
			}
		});
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({
			json: [
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
			]
		});
	});
	await page.route('**/api/recordings/camera-1?date=2026-08-10', async (route) => {
		await route.fulfill({
			json: {
				camera_id: 'camera-1',
				date: '2026-08-10',
				dates: ['2026-08-10'],
				segments: [
					{
						stream: 'main',
						date: '2026-08-10',
						hour: '00',
						filename: '0000.mp4',
						url: '/recording.mp4',
						start_time_ms: Date.UTC(2026, 7, 10),
						end_time_ms: Date.UTC(2026, 7, 10, 0, 10),
						duration_ms: 10 * 60_000
					}
				]
			}
		});
	});
	await page.route('**/api/recordings/camera-1/main/activity', async (route) => {
		await route.fulfill({ status: 204 });
	});
	await page.route('**/recording.mp4', async (route) => {
		await route.fulfill({ status: 200, contentType: 'video/mp4', body: Buffer.alloc(0) });
	});
	await page.route('**/api/events/camera-1?date=2026-08-10', async (route) => {
		await route.fulfill({
			json: {
				camera_id: 'camera-1',
				date: '2026-08-10',
				events: [
					{
						id: 'event-1',
						source: 'camera',
						kind: 'motion',
						start_time_ms: Date.UTC(2026, 7, 10, 0, 5),
						end_time_ms: Date.UTC(2026, 7, 10, 0, 5, 15),
						confidence: null,
						bbox: null,
						zone: null,
						thumbnail_url: '/api/events/camera-1/event-1/thumbnail'
					}
				]
			}
		});
	});
	await page.route('**/api/events/camera-1/event-1/thumbnail', async (route) => {
		await route.fulfill({ status: 200, contentType: 'image/jpeg', body: jpeg });
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
