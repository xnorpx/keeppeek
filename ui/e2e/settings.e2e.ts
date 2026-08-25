import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const storage = {
	medium_term_path: '/recordings/medium',
	long_term_path: '/recordings/long',
	recording_catalog_path: '/recordings/long/recordings.db',
	event_thumbnail_path: '/recordings/long/.event-thumbnails',
	event_thumbnail_max_mb: 1024,
	short_term_secs: 120,
	medium_term_secs: 1800,
	flush_interval_secs: 60,
	write_buffer_bytes: 8192,
	long_term_max_gb: 0
};

const recordingEstimate = {
	estimated_bitrate_bps: 8_576_000,
	bytes_per_day: 92_620_800_000,
	known_streams: 2,
	unknown_streams: 0,
	estimated_retention_days: 2
};

test('redirects the retired Camera defaults bookmark to the Cameras fleet', async ({ page }) => {
	await mockControlPeer(page, { cameras: [], health: { cameras: [] } });

	await page.goto('/settings#camera-defaults');

	await expect(page).toHaveURL(/\/cameras$/);
	await expect(page.getByRole('heading', { name: 'Cameras', exact: true })).toBeVisible();
	await expect(page.getByText('No cameras configured.')).toBeVisible();
});

test('uses a searchable nine-section mobile administration index with focused owners', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const writes: string[] = [];
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			writes.push(`${request.method()} ${request.url()}`);
		}
	});
	await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 2,
			storage: { ...storage, long_term_max_gb: 2048 },
			recording_estimate: { ...recordingEstimate, estimated_retention_days: 25.4 }
		},
		health: {
			version: '0.4.1-test',
			system: { disks: [] },
			storage: { long_term_max_bytes: 2_199_023_255_552, catalog: null }
		}
	});

	await page.goto('/settings');

	const index = page.locator('[data-mobile-settings-index]');
	const navigation = page.getByRole('navigation', { name: 'Settings sections' });
	await expect(index).toBeInViewport();
	await expect(
		page.locator('[data-mobile-settings-header]').getByRole('heading', { name: 'More' })
	).toBeVisible();
	await expect(navigation.getByRole('link')).toHaveCount(9);
	for (const label of [
		'Storage & retention',
		'Event sources',
		'Groups',
		'Notifications',
		'Access',
		'Integrations',
		'Appearance & time',
		'System & updates',
		'Logs & diagnostics'
	]) {
		await expect(navigation.getByRole('link', { name: new RegExp(label) })).toBeVisible();
	}
	await expect(index).toContainText('25 days');
	await expect(index.getByText('—')).toHaveCount(4);
	await expect(page.getByRole('region', { name: 'Storage & retention' })).toBeHidden();

	await page.getByLabel('Search settings').fill('MQTT');
	await expect(navigation.getByRole('link')).toHaveCount(1);
	await expect(navigation.getByRole('link', { name: /Integrations/ })).toBeVisible();
	await page.getByLabel('Search settings').fill('');

	await navigation.getByRole('link', { name: /Access/ }).click();
	await expect(page).toHaveURL(/\/settings#access$/);
	await expect(page.locator('[data-mobile-settings-index]')).toHaveCount(0);
	await expect(page.locator('[data-mobile-settings-focus]')).toContainText('Access');
	await expect(page.locator('[data-mobile-settings-focus]')).toBeInViewport();
	const mobileAccess = page.locator('[data-mobile-access]');
	await expect(mobileAccess).toBeVisible();
	await expect(mobileAccess).toContainText('Identity runtime unavailable');
	await expect(mobileAccess).toContainText('Identity directory unavailable');
	await expect(mobileAccess).toContainText('Token registry unavailable');
	await expect(page.getByRole('button', { name: 'Reveal key' })).toHaveCount(1);
	await expect(mobileAccess.getByRole('button', { name: 'Reveal key' })).toBeEnabled();
	await expect(page.locator('[data-shell-mobile-nav]')).toHaveCount(0);
	await expect(page.locator('[data-mobile-settings-action-bar]')).toContainText(
		'Server update required · keeppeek.identity.v1'
	);
	await expect(page.getByRole('region', { name: 'Storage & retention' })).toBeHidden();

	await page.getByRole('link', { name: 'Back to settings sections' }).click();
	await expect(page).toHaveURL(/\/settings$/);
	await expect(page.locator('[data-mobile-settings-index]')).toBeVisible();
	await page
		.getByRole('navigation', { name: 'Settings sections' })
		.getByRole('link', { name: /Storage & retention/ })
		.click();
	await page.getByRole('button', { name: 'Change storage' }).click();
	await expect(page.getByLabel('Folder path')).toBeVisible();
	await expect(page.getByLabel('Folder path')).toBeFocused();
	await expect(page.getByLabel('Folder path')).toBeInViewport();
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('reveals and rotates the shared access key only after explicit local actions', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const controls = await mockControlPeer(page, {
		accessKey: '550e8400-e29b-41d4-a716-446655440000',
		rotatedAccessKey: '3d813cbb-47fb-4a95-953d-1339b8ff7f54',
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 0,
			storage,
			recording_estimate: recordingEstimate
		}
	});

	await page.goto('/settings#access');
	const access = page.locator('#access');
	await expect(access).toBeVisible();
	await expect(
		access.getByText('550e8400-e29b-41d4-a716-446655440000', { exact: true })
	).toHaveCount(0);
	await expect(
		access.getByText('3d813cbb-47fb-4a95-953d-1339b8ff7f54', { exact: true })
	).toHaveCount(0);

	await access.getByRole('button', { name: 'Reveal key' }).click();
	await expect(
		access.getByText('550e8400-e29b-41d4-a716-446655440000', { exact: true })
	).toBeVisible();
	expect(controls.accessKeyReveals).toBe(1);

	await access.getByRole('button', { name: 'Rotate key' }).click();
	await expect(access.getByRole('button', { name: 'Confirm rotation' })).toBeVisible();
	expect(controls.accessKeyRotations).toBe(0);
	await access.getByRole('button', { name: 'Confirm rotation' }).click();
	await expect(
		access.getByText('3d813cbb-47fb-4a95-953d-1339b8ff7f54', { exact: true })
	).toBeVisible();
	expect(controls.accessKeyRotations).toBe(1);
	await expect(access).toContainText(
		'The shared bootstrap key above is the only implemented key control.'
	);
});

test('Board 20 uses real theme, runtime, restart, and log evidence without inventing system controls', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'reduce' });
	const controls = await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 0,
			storage,
			recording_estimate: recordingEstimate
		},
		health: {
			version: '0.4.1-test',
			uptime_seconds: 183_840,
			system: {
				host_name: 'keeppeek.local',
				os_name: 'macOS',
				os_version: 'macOS 15.4',
				disks: [],
				process: {
					name: 'keeppeek',
					executable: '/opt/keeppeek/bin/keeppeek',
					working_directory: '/opt/keeppeek'
				}
			},
			storage: { long_term_max_bytes: 0, catalog: null }
		},
		cameraCatalog: {
			version: '2.1.0-test',
			tag: 'v2.1.0-test',
			generated_at: '2026-08-22T06:13:00Z',
			camera_count: 3433,
			website_url: 'https://www.cctv-database.com/'
		}
	});
	await page.route('**/metrics', async (route) => {
		await route.fulfill({ contentType: 'text/plain', body: '# EOF\n' });
	});

	await page.goto('/settings#appearance');

	const section = page.getByRole('region', { name: 'The last three settings sections' });
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section).toContainText('PRE-1.0 · 0.4.1-test');
	await expect(section).toContainText('keeppeek.local');
	await expect(section).toContainText('macOS 15.4');
	await expect(section).toContainText('2d 3h 4m');
	await expect(section).toContainText('/opt/keeppeek/bin/keeppeek');
	await expect(section).toContainText('/opt/keeppeek');
	await expect(section).toContainText('2.1.0-test');
	await expect(section).toContainText('3,433');
	await expect(section).toContainText('2026-08-22T06:13:00Z');
	await expect(section.getByRole('link', { name: 'CCTV Database' })).toHaveAttribute(
		'href',
		'https://www.cctv-database.com/'
	);
	await expect(
		section.getByText('Browser reduced motion', { exact: true }).locator('..')
	).toContainText('Reduce');

	await section.getByRole('button', { name: 'Match system' }).click();
	await expect(page.locator('html')).toHaveAttribute('data-theme-preference', 'system');
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
	await expect
		.poll(() => page.evaluate(() => localStorage.getItem('keeppeek-theme')))
		.toBe('system');
	await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
	await expect(page.locator('html')).toHaveClass(/dark/);

	for (const command of [
		'Update check unavailable',
		'Config backup unavailable',
		'Erase unavailable'
	]) {
		await expect(section.getByRole('button', { name: command })).toBeDisabled();
	}
	await expect(section.getByRole('button', { name: 'Download diagnostics' })).toBeEnabled();
	await expect(section.getByRole('link', { name: 'Open logs' })).toHaveAttribute(
		'href',
		'/settings/logs'
	);
	await expect(section.getByRole('link', { name: 'Open health' })).toHaveAttribute(
		'href',
		'/system-health'
	);
	await expect(section).toContainText('SCRUBBED DIAGNOSTICS PACKAGE');
	await expect(section.getByText('Europe/Stockholm', { exact: true })).toHaveCount(0);
	await expect(section.getByText('Stable', { exact: true })).toHaveCount(0);
	await expect(section.getByText('~/.config/keeppeek.toml', { exact: true })).toHaveCount(0);

	await section.getByRole('button', { name: 'Restart', exact: true }).click();
	await expect.poll(() => controls.restarts).toBe(1);
	await expect(page).toHaveURL(/\/settings#appearance$/);
	await expect(page.locator('html')).toHaveAttribute('data-theme-preference', 'system');
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('Board 13 shows measured storage evidence without presenting projected retention as history', async ({
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
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 2,
			storage: {
				...storage,
				medium_term_path: '/recordings/active',
				long_term_path: '/recordings/archive',
				short_term_secs: 90,
				medium_term_secs: 1800,
				long_term_max_gb: 2048
			},
			recording_estimate: {
				...recordingEstimate,
				estimated_retention_days: 25.4
			}
		},
		health: {
			system: {
				disks: [
					{
						name: 'recordings',
						kind: 'ssd',
						file_system: 'apfs',
						mount_point: '/recordings',
						total_bytes: 4_000_000_000_000,
						available_bytes: 1_500_000_000_000,
						used_bytes: 2_500_000_000_000,
						removable: false,
						stores_recordings: true
					}
				]
			},
			storage: {
				long_term_max_bytes: 2_199_023_255_552,
				catalog_bytes: 8_388_608,
				catalog: {
					fragment_bytes: 1_800_000_000_000,
					event_thumbnails: 350,
					oldest_recording_at_ms: Date.UTC(2026, 7, 1),
					newest_recording_at_ms: Date.UTC(2026, 7, 24, 12)
				}
			}
		}
	});

	await page.goto('/settings#storage');

	const section = page.getByRole('region', { name: 'Storage & retention' });
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section).toContainText('25.4 days');
	await expect(section).toContainText('PROJECTED AT CONFIGURED CAP');
	await expect(section).toContainText('2.5 TB USED · 1.5 TB FREE · 4 TB TOTAL');
	await expect(section).toContainText('Indexed fragments 1.8 TB');
	await expect(section).toContainText('Catalog 8.39 MB');
	await expect(section).toContainText('Thumbnails 350');
	await expect(section).toContainText('Short-term buffer');
	await expect(section).toContainText('90 seconds');
	await expect(section).toContainText('Rolling MP4 segment');
	await expect(section).toContainText('30 minutes');
	await expect(section).toContainText('This duration sizes active files; it is not retention age.');
	await expect(section).toContainText('Prune the oldest dated recordings');
	await expect(section).toContainText('below 10% free');
	await expect(section).toContainText('Actual oldest footage');
	await expect(section.locator('time')).toHaveAttribute('datetime', '2026-08-01T00:00:00.000Z');
	await expect(section).toContainText('23.5 days of indexed footage observed');
	await expect(section).toContainText('Server update required · keeppeek.offsite-archive.v1');
	await expect(section.getByText('11 days', { exact: true })).toHaveCount(0);
	await expect(section.getByText('OLDEST FOOTAGE ON DISK', { exact: true })).toHaveCount(0);

	await section.getByRole('button', { name: 'Change storage' }).click();
	await expect(page.getByLabel('Folder path')).toBeInViewport();
	await expect(page.getByLabel('Folder path')).toHaveValue('/recordings/archive');
	await expect(page.getByLabel('Host')).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Edit storage' })).toHaveCount(0);
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('keeps camera discovery and configuration out of Settings', async ({ page }) => {
	const controls = await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 1,
			storage,
			recording_estimate: recordingEstimate
		},
		cameraSettings: [
			{
				id: '192.0.2.10',
				ip: '192.0.2.10',
				display_name: 'North Garden',
				manufacturer_override: null,
				username_configured: true,
				password_configured: true,
				onvif_port: 80,
				http_port: null,
				main_rtsp_url: null,
				sub_rtsp_url: null,
				uid_configured: false,
				backend: 'retina',
				transport: 'tcp',
				record_generic_motion_events: false,
				recording_mode: 'event-boost',
				event_recording_duration_secs: 60,
				health: 'healthy',
				model: 'Test Camera'
			}
		],
		discoveredCameras: [
			{
				ip: '192.0.2.10',
				brand: 'onvif',
				name: 'North Garden',
				model: 'Test Camera',
				onvif_port: 80,
				sources: ['onvif'],
				configured: true,
				health: 'healthy'
			},
			{
				ip: '192.0.2.77',
				brand: 'reolink',
				name: 'Front Gate',
				model: 'RLC-Test',
				onvif_port: null,
				sources: ['onvif', 'baichuan'],
				configured: false,
				health: null
			}
		],
		cameraUpdateResult: {
			camera: {
				id: '192.0.2.77',
				ip: '192.0.2.77',
				display_name: 'Front Gate',
				manufacturer_override: null,
				username_configured: true,
				password_configured: true,
				onvif_port: 80,
				http_port: null,
				main_rtsp_url: 'rtsp://192.0.2.77:8554/live/main',
				sub_rtsp_url: 'rtsp://192.0.2.77:8554/live/sub',
				uid_configured: false,
				backend: 'reo-proto',
				transport: 'tcp',
				record_generic_motion_events: true,
				recording_mode: 'event-boost',
				event_recording_duration_secs: 60,
				health: null,
				model: null
			},
			restart_required: true
		}
	});
	await page.goto('/settings');

	await expect(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Camera setup' })).toHaveCount(0);
	await expect(page.getByLabel('Subnet prefixes')).toHaveCount(0);
	await expect(page.getByRole('heading', { name: 'Edit camera' })).toHaveCount(0);
	expect(controls.discoveryNetworks).toEqual([]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('keeps per-camera recording policy out of Settings', async ({ page }) => {
	const camera = {
		id: '192.0.2.10',
		ip: '192.0.2.10',
		display_name: 'North Garden',
		manufacturer_override: null,
		username_configured: true,
		password_configured: true,
		onvif_port: 80,
		http_port: null,
		main_rtsp_url: null,
		sub_rtsp_url: null,
		uid_configured: false,
		backend: 'retina' as const,
		transport: 'tcp' as const,
		record_generic_motion_events: false,
		recording_mode: 'event-boost' as const,
		event_recording_duration_secs: 60,
		health: 'healthy' as const,
		model: 'Test Camera'
	};
	const controls = await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 1,
			storage: { ...storage, long_term_max_gb: 1024 },
			recording_estimate: recordingEstimate
		},
		cameraSettings: [camera],
		cameraUpdateResult: {
			camera: { ...camera, recording_mode: 'off' },
			restart_required: true
		}
	});
	await page.goto('/settings');

	await expect(page.locator('#camera-recording-mode')).toHaveCount(0);
	await expect(page.getByLabel('Main recording after an event (seconds)')).toHaveCount(0);
	expect(controls.cameraUpdates).toEqual([]);
});

test('reviews and stages safe storage changes before a restart', async ({ page }) => {
	const updatedStorage = {
		medium_term_path: '/archive/medium',
		long_term_path: '/archive/long',
		recording_catalog_path: '/archive/metadata/recordings.db',
		event_thumbnail_path: '/archive/events',
		event_thumbnail_max_mb: 512,
		short_term_secs: 30,
		medium_term_secs: 120,
		flush_interval_secs: 15,
		write_buffer_bytes: 16_384,
		long_term_max_gb: 24
	};
	const controls = await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 0,
			storage,
			recording_estimate: recordingEstimate
		},
		runtimeUpdateResult: {
			config: {
				host: '0.0.0.0',
				port: 3000,
				camera_count: 0,
				storage: updatedStorage,
				recording_estimate: recordingEstimate
			},
			restart_required: true
		},
		health: {
			system: {
				disks: [
					{
						name: 'Current recordings',
						kind: 'SSD',
						file_system: 'apfs',
						mount_point: '/recordings',
						total_bytes: 4_000_000_000_000,
						available_bytes: 1_000_000_000_000,
						used_bytes: 3_000_000_000_000,
						removable: false,
						stores_recordings: true
					},
					{
						name: 'Archive',
						kind: 'SSD',
						file_system: 'apfs',
						mount_point: '/archive',
						total_bytes: 2_000_000_000_000,
						available_bytes: 1_500_000_000_000,
						used_bytes: 500_000_000_000,
						removable: true,
						stores_recordings: false
					}
				]
			},
			storage: {
				long_term_max_bytes: 0,
				catalog: { fragment_bytes: 900_000_000_000 }
			}
		}
	});

	await page.goto('/settings');
	await page.getByRole('button', { name: 'Change storage' }).click();
	await expect(page.getByRole('heading', { name: 'Change recording storage' })).toBeVisible();
	await expect(page.getByLabel('Host')).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Continue to review' })).toBeDisabled();

	await page.getByLabel('Maximum recording storage (GiB)').fill('-1');
	await expect(page.getByText('Maximum recording storage must be a whole number.')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Continue to review' })).toBeDisabled();
	await page.getByLabel('Maximum recording storage (GiB)').fill('24');
	await expect(page.getByText('7 hours', { exact: true })).toBeVisible();
	await page.getByLabel('Folder path').fill('/archive/long');
	await expect(page.getByText('Existing files', { exact: true })).toBeVisible();
	await page.getByLabel('Move existing storage during restart').check();

	await page.getByText('Advanced storage paths and writer controls').click();
	await page.getByLabel('Active recording path').fill('/archive/medium');
	await page.getByLabel('Recording catalog path').fill('/archive/metadata/recordings.db');
	await page.getByLabel('Event thumbnail path').fill('/archive/events');
	await page.getByLabel('Thumbnail storage limit (MiB)').fill('512');
	await page.getByLabel('Memory buffer (seconds)').fill('30');
	await page.getByLabel('Recording file duration (seconds)').fill('120');
	await page.getByLabel('Flush interval (seconds)').fill('15');
	await page.getByLabel('Write buffer (bytes)').fill('16384');
	await page.getByRole('button', { name: 'Continue to review' }).click();

	await expect(page.getByRole('heading', { name: 'Review storage changes' })).toBeFocused();
	await expect(page.getByText('Move during restart', { exact: true })).toBeVisible();
	await expect(page.getByText('RESTART REQUIRED', { exact: true })).toBeVisible();
	await expect(page.getByText(/may remove about 814 GiB of indexed footage/)).toBeVisible();
	await expect(page.getByText('/archive/metadata/recordings.db', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Stage storage changes' }).click();

	await expect(
		page.getByText(
			'Storage settings staged. Restart will move existing storage before recording resumes.',
			{ exact: true }
		)
	).toBeVisible();
	await expect(page.getByRole('button', { name: 'Restart and move storage' })).toBeVisible();
	expect(controls.runtimeUpdates).toEqual([
		{
			host: '0.0.0.0',
			port: 3000,
			storage: updatedStorage,
			move_existing_recordings: true
		}
	]);
	expect(controls.restarts).toBe(0);
});

test('protects an unsaved storage draft from cancel and navigation', async ({ page }) => {
	await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 0,
			storage: { ...storage, long_term_max_gb: 1024 },
			recording_estimate: recordingEstimate
		}
	});
	await page.goto('/settings#storage');
	await page.getByRole('button', { name: 'Change storage' }).click();
	await page.getByLabel('Maximum recording storage (GiB)').fill('512');
	await expect(page.getByText('1 unsaved change', { exact: true })).toBeVisible();

	let dialogPromise = page.waitForEvent('dialog');
	let actionPromise = page.getByRole('button', { name: 'Cancel', exact: true }).click();
	let dialog = await dialogPromise;
	expect(dialog.message()).toBe('Discard your unsaved storage changes?');
	await dialog.dismiss();
	await actionPromise;
	await expect(page.getByRole('heading', { name: 'Change recording storage' })).toBeVisible();

	dialogPromise = page.waitForEvent('dialog');
	actionPromise = page.getByRole('link', { name: 'View logs' }).click();
	dialog = await dialogPromise;
	expect(dialog.message()).toBe('Discard your unsaved storage changes?');
	await dialog.dismiss();
	await actionPromise;
	await expect(page).toHaveURL(/\/settings#storage$/);
	await expect(page.getByLabel('Maximum recording storage (GiB)')).toHaveValue('512');

	dialogPromise = page.waitForEvent('dialog');
	actionPromise = page.getByRole('button', { name: 'Cancel', exact: true }).click();
	dialog = await dialogPromise;
	await dialog.accept();
	await actionPromise;
	await expect(page.getByRole('heading', { name: 'Change recording storage' })).toHaveCount(0);
});

test('blocks a storage move that cannot fit on the reported destination', async ({ page }) => {
	await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 0,
			storage: { ...storage, long_term_max_gb: 1024 },
			recording_estimate: recordingEstimate
		},
		health: {
			system: {
				disks: [
					{
						name: 'Current recordings',
						kind: 'SSD',
						file_system: 'apfs',
						mount_point: '/recordings',
						total_bytes: 2_000_000_000_000,
						available_bytes: 1_000_000_000_000,
						used_bytes: 1_000_000_000_000,
						removable: false,
						stores_recordings: true
					},
					{
						name: 'Small archive',
						kind: 'SSD',
						file_system: 'apfs',
						mount_point: '/archive',
						total_bytes: 500_000_000_000,
						available_bytes: 100_000_000_000,
						used_bytes: 400_000_000_000,
						removable: true,
						stores_recordings: false
					}
				]
			},
			storage: {
				long_term_max_bytes: 1_099_511_627_776,
				catalog: { fragment_bytes: 900_000_000_000 }
			}
		}
	});
	await page.goto('/settings#storage');
	await page.getByRole('button', { name: 'Change storage' }).click();
	await page.getByLabel('Folder path').fill('/archive/recordings');
	await page.getByLabel('Move existing storage during restart').check();

	await expect(page.getByText(/destination has 93 GiB free, less than the 838 GiB/)).toBeVisible();
	await expect(page.getByRole('button', { name: 'Continue to review' })).toBeDisabled();
	await page.getByLabel('Use the new location from restart').check();
	await expect(page.getByText(/destination has 93 GiB free/)).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Continue to review' })).toBeEnabled();
});

test('keeps storage setup and review inside the mobile administration viewport', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 0,
			storage: { ...storage, long_term_max_gb: 1024 },
			recording_estimate: recordingEstimate
		}
	});
	await page.goto('/settings?edit=storage#storage');

	await expect(page.getByRole('heading', { name: 'Change recording storage' })).toBeVisible();
	await expect(page.getByLabel('Folder path')).toBeInViewport();
	await page.getByLabel('Maximum recording storage (GiB)').fill('512');
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	await page.getByRole('button', { name: 'Continue to review' }).click();
	await expect(page.getByRole('heading', { name: 'Review storage changes' })).toBeFocused();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('uses the browser hostname after a wildcard host changes port', async ({ page }) => {
	const targetOrigin = 'http://127.0.0.1:3201';
	const controls = await mockControlPeer(page, {
		runtimeConfiguration: {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 0,
			storage,
			recording_estimate: recordingEstimate
		},
		runtimeUpdateResult: {
			config: {
				host: '0.0.0.0',
				port: 3201,
				camera_count: 0,
				storage,
				recording_estimate: recordingEstimate
			},
			restart_required: true
		}
	});
	await page.route(`${targetOrigin}/metrics`, async (route) => {
		await route.fulfill({
			contentType: 'text/plain',
			body: '# EOF\n'
		});
	});
	await page.route(`${targetOrigin}/settings`, async (route) => {
		await route.fulfill({ contentType: 'text/html', body: '<main>Wildcard restart</main>' });
	});

	await page.goto('/settings');
	await page.getByRole('button', { name: 'Edit server' }).click();
	await page.getByLabel('Port').fill('3201');
	await page.getByRole('button', { name: 'Save server settings' }).click();
	await page.getByRole('button', { name: 'Apply changes' }).click();

	await expect(page).toHaveURL(`${targetOrigin}/settings`);
	await expect(page.getByText('Wildcard restart', { exact: true })).toBeVisible();
	expect(controls.runtimeUpdates).toEqual([
		{
			host: '0.0.0.0',
			port: 3201,
			storage,
			move_existing_recordings: false
		}
	]);
	expect(controls.restarts).toBe(1);
});

test('keeps confirmed settings visible and locked while a WebRTC update is applying', async ({
	page
}) => {
	let releaseUpdate!: () => void;
	const runtimeUpdateGate = new Promise<void>((resolve) => {
		releaseUpdate = resolve;
	});
	const updatedConfig = {
		host: '0.0.0.0',
		port: 3201,
		camera_count: 0,
		storage,
		recording_estimate: recordingEstimate
	};
	const controls = await mockControlPeer(page, {
		runtimeConfiguration: { ...updatedConfig, port: 3000 },
		runtimeUpdateGate,
		runtimeUpdateResult: { config: updatedConfig, restart_required: false }
	});
	await page.goto('/settings');
	await page.getByRole('button', { name: 'Edit server' }).click();
	const port = page.getByLabel('Port');
	await port.fill('3201');
	await page.getByRole('button', { name: 'Save server settings' }).click();

	const applying = page.locator('[data-settings-applying]');
	await expect(applying).toContainText('Applying server settings');
	await expect(applying).toContainText('Confirmed values remain visible');
	await expect(port).toHaveValue('3000');
	await expect(port).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Cancel' }).last()).toBeDisabled();
	expect(controls.runtimeUpdates).toHaveLength(1);

	releaseUpdate();
	await expect(applying).toHaveCount(0);
	await expect(page.getByText('Server settings saved.', { exact: true })).toBeVisible();
	await expect(page.getByText('3201', { exact: true })).toBeVisible();
});
