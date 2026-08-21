import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import type { CameraSettings } from '../src/lib/types';

const config = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: 3,
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
		long_term_max_gb: 2048
	},
	recording_estimate: {
		estimated_bitrate_bps: 8_576_000,
		bytes_per_day: 92_620_800_000,
		known_streams: 3,
		unknown_streams: 0,
		estimated_retention_days: 14
	}
};

const cameras: CameraSettings[] = [
	{
		id: 'workshop',
		ip: '192.0.2.10',
		display_name: 'Workshop',
		manufacturer_override: null,
		username_configured: true,
		password_configured: true,
		onvif_port: 8000,
		http_port: 80,
		main_rtsp_url: null,
		sub_rtsp_url: null,
		uid_configured: false,
		backend: 'auto',
		transport: 'tcp',
		health: 'online',
		model: 'RLC-Test'
	},
	{
		id: 'till',
		ip: '192.0.2.11',
		display_name: 'Till',
		manufacturer_override: null,
		username_configured: true,
		password_configured: false,
		onvif_port: null,
		http_port: null,
		main_rtsp_url: 'rtsp://192.0.2.11/main',
		sub_rtsp_url: null,
		uid_configured: false,
		backend: 'retina',
		transport: 'udp',
		health: 'degraded',
		model: null
	},
	{
		id: 'porch',
		ip: '192.0.2.12',
		display_name: 'Porch',
		manufacturer_override: null,
		username_configured: false,
		password_configured: false,
		onvif_port: null,
		http_port: null,
		main_rtsp_url: null,
		sub_rtsp_url: null,
		uid_configured: true,
		backend: 'reo-proto',
		transport: 'tcp',
		health: 'offline',
		model: null
	}
];

async function mockSettings(page: Page): Promise<string[]> {
	const writes: string[] = [];
	await mockControlPeer(page, { runtimeConfiguration: config, cameraSettings: cameras });
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			writes.push(`${request.method()} ${request.url()}`);
		}
	});
	return writes;
}

test('reports real camera-default evidence without inventing shared credentials', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const writes = await mockSettings(page);

	await page.goto('/settings#camera-defaults');

	const section = page.getByRole('region', { name: 'Camera defaults' });
	await expect(section).toBeVisible();
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(page).toHaveURL(/\/settings#camera-defaults$/);
	await expect(section.getByText('3', { exact: true }).first()).toBeVisible();
	await expect(section.getByText('CAMERAS OBSERVED')).toBeVisible();
	await expect(section.getByText('SHARED INHERITANCE NOT EXPOSED')).toBeVisible();
	await expect(section.getByText('Save camera defaults')).toHaveCount(0);
	await expect(
		section.locator('[data-capability-gate][data-capability="keeppeek.runtime-config.v1"]')
	).toHaveCount(0);

	for (const label of ['Complete', 'Partial', 'Missing']) {
		const summary = section.getByText(label, { exact: true }).locator('..');
		await expect(summary.getByText('1', { exact: true })).toBeVisible();
	}
	await expect(section).toContainText('Auto 1 · Retina 1 · Reo-Proto 1');
	await expect(section).toContainText('TCP 2 · UDP 1');
	await expect(section).toContainText('1 cameras');
	await expect(section.getByText('NOT EXPOSED PER CAMERA')).toBeVisible();
	await expect(section).toContainText('About 14 days · 2048 GB cap');

	await expect(section.getByText('Workshop', { exact: true })).toBeVisible();
	await expect(section.getByText('Till', { exact: true })).toBeVisible();
	await expect(section.getByText('Porch', { exact: true })).toBeVisible();
	await expect(section.getByText('Username + password configured')).toBeVisible();
	await expect(section.getByText('Partial credentials')).toBeVisible();
	await expect(section.getByText('No credentials configured')).toBeVisible();
	await expect(section.locator('input[type="password"]')).toHaveCount(0);
	await expect(section.getByText('admin', { exact: true })).toHaveCount(0);
	await expect(section.getByText('write-only-password', { exact: true })).toHaveCount(0);
	expect(writes).toEqual([]);
});

test('renders Board 27 mobile camera defaults without inventing shared credentials', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const writes = await mockSettings(page);

	await page.goto('/settings#camera-defaults');

	const header = page.locator('[data-mobile-settings-header]');
	const section = page.locator('[data-mobile-camera-defaults]');
	const actionBar = page.locator('[data-mobile-settings-action-bar]');
	await expect(header).toContainText('Camera defaults');
	await expect(header).toContainText('Save · Server update required');
	await expect(section).toBeVisible();
	await expect(section).toContainText('Not returned by the API');
	await expect(section).toContainText('Write-only per camera');
	await expect(section).toContainText('Not exposed');
	await expect(section.getByText('Workshop', { exact: true })).toBeVisible();
	await expect(section.getByText('Till', { exact: true })).toBeVisible();
	await expect(section.getByText('Porch', { exact: true })).toBeVisible();
	await expect(section.getByText('admin', { exact: true })).toHaveCount(0);
	await expect(section.locator('input[type="password"]')).toHaveCount(0);
	await expect(actionBar).toContainText('Server update required · keeppeek.runtime-config.v1');
	expect(
		await page
			.locator('[data-mobile-camera-defaults]')
			.evaluate((element) => Math.round(element.getBoundingClientRect().height))
	).toBe(660);
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
