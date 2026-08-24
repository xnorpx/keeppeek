import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import type { CameraSettings } from '../src/lib/types';

const config = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: 2,
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
		estimated_bitrate_bps: 8_000_000,
		bytes_per_day: 86_400_000_000,
		known_streams: 2,
		unknown_streams: 0,
		estimated_retention_days: 25.4
	}
};

const cameras: CameraSettings[] = ['Front Door', 'Back Yard'].map((name, index) => ({
	id: `camera-${index}`,
	ip: `192.0.2.${10 + index}`,
	display_name: name,
	manufacturer_override: null,
	username_configured: true,
	password_configured: true,
	onvif_port: null,
	http_port: null,
	main_rtsp_url: null,
	sub_rtsp_url: null,
	uid_configured: false,
	backend: 'auto',
	transport: 'tcp',
	record_generic_motion_events: false,
	recording_mode: 'event-boost',
	event_recording_duration_secs: 60,
	health: 'online',
	model: null
}));

async function openGroups(page: Page): Promise<string[]> {
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
		cameraSettings: cameras,
		health: {
			system: { disks: [] },
			storage: { long_term_max_bytes: 2_199_023_255_552, catalog: null }
		}
	});

	await page.goto('/settings#groups');
	return writes;
}

test('Board 19 gates group administration without fabricating a directory', async ({ page }) => {
	const writes = await openGroups(page);

	const section = page.getByRole('region', { name: 'Groups & two-way audio' });
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section).toContainText('GROUPS · SERVER CONFIGURATION');
	await expect(section).toContainText('New group');
	await expect(section).toContainText('Server update required · keeppeek.group-admin.v1');
	await expect(section).toContainText('Manage group definitions');
	await expect(section).toContainText('Group directory unavailable');
	await expect(section).toContainText('COUNT UNAVAILABLE');
	await expect(section).toContainText('GENERATED TYPES · NO RUNTIME HANDLER');

	for (const command of ['list', 'join', 'leave']) {
		await expect(section.getByText(command, { exact: true })).toBeVisible();
	}
	await expect(section).toContainText('Static camera streams only');
	await expect(section).toContainText('Optional · never returned');
	await expect(section).toContainText('Deliberately absent');
	await expect(section).toContainText('Always full duplex');
	await expect(section).toContainText('No floor control.');
	await expect(section).toContainText('Push-to-talk is local.');
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('Board 19 keeps participant state unavailable until a real group join exists', async ({
	page
}) => {
	const writes = await openGroups(page);
	const section = page.getByRole('region', { name: 'Groups & two-way audio' });

	await expect(section).toContainText('Participant state is authoritative.');
	await expect(section).toContainText(
		'it cannot be inferred from cameras or WebRTC session totals'
	);

	for (const absent of [
		'Front of house',
		'Yard & perimeter',
		'Shop floor intercom',
		'Marcus',
		'Anna',
		'2 talking'
	]) {
		await expect(section.getByText(absent, { exact: true })).toHaveCount(0);
	}
	await expect(section.getByText('Front Door', { exact: true })).toHaveCount(0);
	await expect(section.getByText('Back Yard', { exact: true })).toHaveCount(0);
	expect(writes).toEqual([]);
});
