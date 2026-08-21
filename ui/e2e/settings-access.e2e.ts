import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const config = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: 0,
	storage: {
		medium_term_path: '/recordings',
		long_term_path: '/recordings',
		recording_catalog_path: '/recordings/recordings.db',
		event_thumbnail_path: '/recordings/.event-thumbnails',
		event_thumbnail_max_mb: 1024,
		short_term_secs: 120,
		medium_term_secs: 1800,
		flush_interval_secs: 60,
		write_buffer_bytes: 1_048_576,
		long_term_max_gb: 2048
	},
	recording_estimate: {
		estimated_bitrate_bps: 0,
		bytes_per_day: 0,
		known_streams: 0,
		unknown_streams: 0,
		estimated_retention_days: null
	}
};

test('Board 16 shows target roles without claiming runtime identity enforcement', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const writes: string[] = [];
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			writes.push(`${request.method()} ${request.url()}`);
		}
	});
	await mockControlPeer(page, {
		runtimeConfiguration: config,
		health: { system: { disks: [] }, storage: { catalog: null } }
	});

	await page.goto('/settings#access');

	const section = page.getByRole('region', { name: 'Operation is not configuration' });
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section).toContainText('No runtime identity or role evidence');
	await expect(section).toContainText(
		'The Rust server enforces loopback Administrator bypass and one shared Bearer key'
	);
	await expect(section).toContainText('Authored policy only · not enforced by this server');
	await expect(section).toContainText('TARGET · IDENTITY.V1');
	await expect(section).toContainText('Invite someone');
	await expect(section).toContainText('New access token');
	await expect(section).toContainText('Turn on remote sign-in');
	await expect(section.getByText('Server update required · keeppeek.identity.v1')).toHaveCount(5);

	for (const action of [
		'Watch live video',
		'Open stored recordings',
		'Operate camera PTZ and presets',
		'Join a group and publish local media',
		'Export a clip or still',
		'Configure cameras',
		'Configure storage and services',
		'Manage identities and tokens'
	]) {
		await expect(section.getByText(action, { exact: true })).toBeVisible();
	}
	await expect(section.locator('[aria-label="Administrator target allows"]')).toHaveCount(8);
	await expect(section.locator('[aria-label="User target allows"]')).toHaveCount(5);
	await expect(section.locator('[aria-label="User target excludes"]')).toHaveCount(3);

	await expect(section).toContainText('Identity directory unavailable');
	await expect(section).toContainText('Token registry unavailable');
	await expect(section.getByText('COUNT UNAVAILABLE')).toHaveCount(2);
	await expect(section).toContainText('IMPLEMENTED LOOPBACK MODEL');
	await expect(section).toContainText('Administrator without sign-in');
	await expect(section).toContainText('IMPLEMENTED REMOTE MODEL');
	await expect(section).toContainText('One shared Bearer key');
	await expect(section).toContainText('AUDIT TRAIL');

	for (const absent of [
		'Marcus',
		'Anna',
		'Workshop tablet',
		'Front desk',
		'Home Assistant card',
		'object-detect',
		'Metrics collector',
		'doorbell-bridge'
	]) {
		await expect(section.getByText(absent, { exact: true })).toHaveCount(0);
	}
	await expect(section.locator('input[type="password"]')).toHaveCount(0);
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('renders Board 27 mobile access without inventing people or tokens', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockControlPeer(page, {
		runtimeConfiguration: config,
		health: { system: { disks: [] }, storage: { catalog: null } }
	});

	await page.goto('/settings#access');

	const header = page.locator('[data-mobile-settings-header]');
	const section = page.locator('[data-mobile-access]');
	const actionBar = page.locator('[data-mobile-settings-action-bar]');
	await expect(header).toContainText('Access');
	await expect(header).toContainText('Target · identity v1');
	await expect(section).toContainText('Identity runtime unavailable');
	await expect(section).toContainText('Identity directory unavailable');
	await expect(section).toContainText('Token registry unavailable');
	await expect(actionBar).toContainText('Server update required · keeppeek.identity.v1');
	for (const unsupportedFixtureText of ['Marcus', 'Anna', 'Workshop tablet', 'object-detect']) {
		await expect(section.getByText(unsupportedFixtureText, { exact: true })).toHaveCount(0);
	}
	expect(
		await section.evaluate((element) => Math.round(element.getBoundingClientRect().height))
	).toBe(660);
	await expect(page.locator('[data-shell-mobile-nav]')).toHaveCount(0);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
