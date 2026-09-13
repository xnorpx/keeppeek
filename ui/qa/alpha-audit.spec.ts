import { expect, test, type Page } from '@playwright/test';
import { mockEvents, eventDate } from '../e2e/fixtures/events';
import { mockKeepModes, keepModeDate } from '../e2e/fixtures/keep-modes';
import { mockControlPeer } from '../e2e/fixtures/control-peer';
import { mixedCameras, mixedHealth } from '../e2e/fixtures/peek';

const runtimeConfiguration = {
	host: '127.0.0.1',
	port: 3000,
	camera_count: mixedCameras.length,
	storage: {
		medium_term_path: '/recordings/medium',
		long_term_path: '/recordings/long',
		recording_catalog_path: '/recordings/long/recordings.db',
		event_thumbnail_path: '/recordings/long/.event-thumbnails',
		event_thumbnail_max_mb: 1024,
		short_term_secs: 120,
		medium_term_secs: 1800,
		flush_interval_secs: 60,
		write_buffer_bytes: 8192,
		long_term_max_gb: 0,
		minimum_free_gb: 10,
		maximum_used_percent: null,
		warning_free_gb: 20,
		critical_free_gb: 10,
		cleanup_hysteresis_gb: 5
	},
	recording_estimate: {
		estimated_bitrate_bps: 0,
		bytes_per_day: 0,
		known_streams: 0,
		unknown_streams: 4,
		estimated_retention_days: null
	}
};

const surfaces = [
	['dashboard', '/'],
	['viewer', '/viewer'],
	['cameras', '/cameras'],
	['onboarding', '/cameras/new'],
	['keep', `/keep?camera=front-door&date=${keepModeDate}`],
	['events', `/events?date=${eventDate}`],
	['health', '/system-health'],
	['settings', '/settings'],
	['storage', '/settings#storage'],
	['access', '/settings#access'],
	['integrations', '/settings#integrations'],
	['logs', '/settings/logs'],
	['maintenance', '/recordings/maintenance']
] as const;

async function waitForSurface(page: Page, surface: string, width: number): Promise<void> {
	if (surface === 'dashboard')
		await expect(page.locator('[data-peek-focus="front-door"]')).toBeVisible();
	else if (surface === 'cameras')
		await expect(page.locator('[data-fleet-row="front-door"]')).toBeVisible();
	else if (surface === 'onboarding')
		await expect(
			page.getByRole('button', { name: width < 768 ? 'Scan this network' : 'Discover cameras' })
		).toBeVisible();
	else if (surface === 'events')
		await expect(page.getByText('5 events', { exact: true })).toBeVisible();
	else if (surface === 'settings' && width < 768)
		await expect(page.getByRole('navigation', { name: 'Settings sections' })).toBeVisible();
	else if (surface === 'logs')
		await expect(page.getByRole('heading', { name: 'Logs', exact: true })).toBeVisible();
	else {
		const regions: Record<string, string> = {
			viewer: 'Front Door focus',
			keep: 'Recorded video player',
			health: width < 768 ? 'Mobile health overview' : 'Camera health dimensions',
			settings: 'Dashboards',
			storage: 'Storage & retention',
			access: 'Access credentials',
			integrations: 'Everything has an explicit egress boundary',
			maintenance: 'Maintenance history'
		};
		await expect(page.getByRole('region', { name: regions[surface], exact: true })).toBeVisible();
	}
}

for (const width of [1440, 390, 320]) {
	for (const [surface, route] of surfaces) {
		test(`${surface} fixture renders without overflow or page errors at ${width}px`, async ({
			page
		}, testInfo) => {
			await page.setViewportSize({ width, height: width === 1440 ? 900 : 844 });
			const errors: string[] = [];
			page.on('pageerror', (error) => errors.push(error.message));
			if (surface === 'logs')
				await page.route(
					(url) => url.pathname === '/logs',
					(route) =>
						route.fulfill({ contentType: 'text/event-stream', body: ': audit fixture\n\n' })
				);
			if (surface === 'events') await mockEvents(page);
			else if (surface === 'keep') await mockKeepModes(page);
			else
				await mockControlPeer(page, {
					cameras: mixedCameras,
					health: mixedHealth,
					runtimeConfiguration
				});
			await page.goto(route);
			await expect(page.locator('[data-shell-main]')).toBeVisible();
			await expect(page.locator('[data-keyboard-ready]')).toHaveAttribute(
				'data-keyboard-ready',
				'true'
			);
			await waitForSurface(page, surface, width);
			await page.screenshot({ path: testInfo.outputPath('surface.png'), fullPage: true });
			await testInfo.attach('accessibility', {
				body: await page.locator('body').ariaSnapshot(),
				contentType: 'text/plain'
			});
			const geometry = await page.evaluate(() => ({
				viewport: innerWidth,
				document: document.documentElement.scrollWidth,
				controls: [...document.querySelectorAll('button, a, input, select')]
					.filter((element) => element.getClientRects().length > 0)
					.map((element) => {
						const box = element.getBoundingClientRect();
						return {
							name: element.getAttribute('aria-label') ?? element.textContent?.trim(),
							tag: element.tagName,
							x: box.x,
							y: box.y,
							width: box.width,
							height: box.height
						};
					})
			}));
			await testInfo.attach('geometry', {
				body: JSON.stringify(geometry, null, 2),
				contentType: 'application/json'
			});
			expect.soft(errors, 'uncaught browser errors').toEqual([]);
			expect.soft(geometry.document, 'horizontal document overflow').toBeLessThanOrEqual(width);
		});
	}
}
