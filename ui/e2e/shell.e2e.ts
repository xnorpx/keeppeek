import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

async function mockEmptyRecorder(page: Page): Promise<void> {
	await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '127.0.0.1',
			port: 3000,
			camera_count: 0,
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
				unknown_streams: 0,
				estimated_retention_days: null
			}
		}
	});
}

test('uses the fixed Paper desktop shell geometry', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockEmptyRecorder(page);
	await page.goto('/');

	const rail = page.getByRole('complementary', { name: 'Desktop navigation' });
	const primaryNavigation = page.getByRole('navigation', { name: 'Primary navigation' });
	const contextBar = page.locator('[data-shell-context]');
	const main = page.locator('[data-shell-main]');
	const statusBar = page.locator('[data-shell-status]');

	await expect(rail).toBeVisible();
	await expect(rail).toHaveCSS('width', '64px');
	await expect(rail.getByRole('link', { name: 'Dashboard' })).toHaveText('KP');
	await expect(rail.getByRole('link', { name: 'Dashboard' })).toHaveAttribute(
		'aria-current',
		'page'
	);
	await expect(contextBar).toBeHidden();
	await expect(main).toHaveCSS('height', '900px');
	await expect(statusBar).toHaveCount(0);
	await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toHaveCount(1);
	await expect(page.getByRole('heading', { name: 'Peek', exact: true })).toHaveCount(0);
	await expect(
		page.locator('[data-shell-rail-actions]').getByRole('button', { name: 'Switch to light theme' })
	).toBeVisible();
	await expect(page.locator('[data-shell-mobile-nav]')).toBeHidden();
	await expect(primaryNavigation.getByRole('link')).toHaveCount(6);
	expect(
		await primaryNavigation
			.getByRole('link')
			.evaluateAll((links) => links.map((link) => link.getAttribute('aria-label')))
	).toEqual(['Viewer', 'Keep', 'Events', 'Cameras', 'Health', 'Settings']);
});

test('keeps primary views inside the desktop shell while settings may scroll', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockEmptyRecorder(page);

	for (const pathname of [
		'/',
		'/viewer',
		'/keep',
		'/recordings',
		'/events',
		'/cameras',
		'/system-health'
	]) {
		await page.goto(pathname);
		const main = page.locator('[data-shell-main]');
		await expect(page.locator('[data-shell-context]')).toBeHidden();
		await expect(page.locator('[data-shell-status]')).toHaveCount(0);
		await expect(main).toHaveCSS('overflow-y', 'hidden');
		const geometry = await main.evaluate((element) => ({
			clientHeight: element.clientHeight,
			scrollHeight: element.scrollHeight
		}));
		expect(geometry.scrollHeight, `${pathname} exceeds the shell viewport`).toBeLessThanOrEqual(
			geometry.clientHeight + 1
		);
	}

	await page.goto('/settings');
	await expect(page.locator('[data-shell-context]')).toBeVisible();
	await expect(page.locator('[data-shell-status]')).toHaveCount(0);
	await expect(page.locator('[data-shell-context]')).toHaveCSS('height', '52px');
	await expect(page.locator('[data-shell-main]')).toHaveCSS('overflow-y', 'auto');
});

test('uses the six-destination mobile shell without horizontal overflow', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockEmptyRecorder(page);
	await page.goto('/');

	const mobileNavigation = page.locator('[data-shell-mobile-nav]');
	const links = mobileNavigation.getByRole('link');

	await expect(page.getByRole('complementary', { name: 'Desktop navigation' })).toBeHidden();
	await expect(mobileNavigation).toBeVisible();
	await expect(mobileNavigation).toHaveCSS('height', '78px');
	await expect(links).toHaveCount(6);
	expect((await links.allTextContents()).map((label) => label.trim())).toEqual([
		'Dashboard',
		'Viewer',
		'Keep',
		'Events',
		'Health',
		'More'
	]);
	await expect(mobileNavigation.getByRole('link', { name: 'Dashboard' })).toHaveAttribute(
		'aria-current',
		'page'
	);
	await expect(page.locator('[data-shell-context]')).toHaveCSS('height', '50px');
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
