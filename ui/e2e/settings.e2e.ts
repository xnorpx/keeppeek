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

test('uses a searchable ten-section mobile administration index with focused owners', async ({
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
		cameraSettings: ['Front Door', 'Back Yard'].map((name, index) => ({
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
			health: 'online',
			model: null
		})),
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
	await expect(navigation.getByRole('link')).toHaveCount(10);
	for (const label of [
		'Camera defaults',
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
	await expect(index).toContainText('2 / 2');
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
		.getByRole('link', { name: /Camera defaults/ })
		.click();
	await expect(page).toHaveURL(/\/settings#camera-defaults$/);
	await expect(page.locator('[data-mobile-settings-focus]')).toContainText('Camera defaults');
	await expect(page.locator('[data-mobile-settings-focus]')).toBeInViewport();
	const mobileCameraDefaults = page.locator('[data-mobile-camera-defaults]');
	await expect(mobileCameraDefaults).toBeVisible();
	await expect(mobileCameraDefaults).toContainText('Not returned by the API');
	await expect(mobileCameraDefaults).toContainText('Write-only per camera');
	await expect(page.locator('[data-shell-mobile-nav]')).toHaveCount(0);
	await expect(page.locator('[data-mobile-settings-action-bar]')).toContainText(
		'Server update required · keeppeek.runtime-config.v1'
	);
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
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
		'Erase unavailable',
		'Diagnostics bundle unavailable'
	]) {
		await expect(section.getByRole('button', { name: command })).toBeDisabled();
	}
	await expect(section.getByRole('link', { name: 'Open logs' })).toHaveAttribute(
		'href',
		'/settings/logs'
	);
	await expect(section.getByRole('link', { name: 'Open health' })).toHaveAttribute(
		'href',
		'/system-health'
	);
	await expect(section).toContainText('FULL DIAGNOSTICS BUNDLE UNAVAILABLE');
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
					event_thumbnails: 350
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
	await expect(section).toContainText('Event thumbnails 350');
	await expect(section).toContainText('Short-term buffer');
	await expect(section).toContainText('90 seconds');
	await expect(section).toContainText('Rolling MP4 segment');
	await expect(section).toContainText('30 minutes');
	await expect(section).toContainText('This duration sizes active files; it is not retention age.');
	await expect(section).toContainText('Prune the oldest dated recordings');
	await expect(section).toContainText('below 10% free');
	await expect(section).toContainText('ACTUAL OLDEST FOOTAGE');
	await expect(section).toContainText('projected retention is never labeled as observed history');
	await expect(section).toContainText('Server update required · keeppeek.offsite-archive.v1');
	await expect(section.getByText('11 days', { exact: true })).toHaveCount(0);
	await expect(section.getByText('OLDEST FOOTAGE ON DISK', { exact: true })).toHaveCount(0);

	await section.getByRole('button', { name: 'Edit runtime storage' }).click();
	await expect(page.getByLabel('Medium-term path')).toBeInViewport();
	await expect(page.getByLabel('Medium-term path')).toHaveValue('/recordings/active');
	await expect(page.getByLabel('Long-term path')).toHaveValue('/recordings/archive');
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('discovers and configures a camera without rendering its saved password', async ({ page }) => {
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
				health: 'online',
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
				health: 'online'
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
				health: null,
				model: null
			},
			restart_required: true
		}
	});
	await page.goto('/settings');

	await expect(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible();
	await expect(page.getByText('online', { exact: true })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Open North Garden live view' })).toHaveAttribute(
		'href',
		'/?camera=192.0.2.10'
	);
	await expect(
		page.getByRole('link', { name: 'Open North Garden camera information' })
	).toHaveAttribute('href', '/camera?camera=192.0.2.10');
	await page.getByLabel('Subnet prefixes').fill('192.168.137, 192.168.138');
	await page.getByRole('button', { name: 'Discover' }).click();
	await expect(page.getByText('Front Gate', { exact: true })).toBeVisible();
	expect(controls.discoverySubnets).toEqual([[137, 138]]);
	await page.getByRole('button', { name: 'Review' }).click();
	await expect(page.getByRole('heading', { name: 'Edit camera' })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Edit camera' })).toBeInViewport();
	await expect(page.getByLabel('IP address')).toHaveValue('192.0.2.10');
	await expect(page.getByLabel('Display name')).toBeFocused();
	await page.getByRole('button', { name: 'Cancel' }).first().click();
	await page.getByRole('button', { name: 'Configure' }).click();
	await expect(page.getByRole('heading', { name: 'Add camera' })).toBeInViewport();
	await expect(page.getByLabel('IP address')).toHaveValue('192.0.2.77');
	await expect(page.getByLabel('Username')).toBeFocused();
	await expect(page.getByLabel('ONVIF port')).toHaveValue('8000');
	await expect(page.getByLabel('HTTP port')).toHaveValue('80');
	await expect(page.getByLabel('Main RTSP stream URL')).toHaveValue('');
	await expect(page.getByLabel('Sub RTSP stream URL')).toHaveValue('');
	await expect(page.getByLabel('Main RTSP stream URL')).not.toHaveAttribute('placeholder');
	await expect(page.getByLabel('Sub RTSP stream URL')).not.toHaveAttribute('placeholder');
	await expect(page.locator('#camera-password')).toHaveAttribute('type', 'password');
	await page.getByLabel('Username').fill('operator');
	await page.getByLabel('Password').fill('write-only-password');
	await page.getByLabel('Main RTSP stream URL').fill('rtsp://192.0.2.77:8554/live/main');
	await page.getByLabel('Sub RTSP stream URL').fill('rtsp://192.0.2.77:8554/live/sub');
	await page.getByRole('button', { name: 'Save camera' }).click();

	await expect(page.getByText('Camera settings saved.', { exact: true })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Apply changes' })).toBeVisible();
	await expect(page.getByText('Credentials saved', { exact: true })).toHaveCount(2);
	await expect(page.getByText('write-only-password', { exact: true })).toHaveCount(0);
	expect(controls.cameraUpdates).toHaveLength(1);
	expect(controls.cameraUpdates[0]).toMatchObject({
		ip: '192.0.2.77',
		update: {
			display_name: 'Front Gate',
			username: 'operator',
			password: 'write-only-password',
			backend: 'reo-proto',
			transport: 'tcp',
			main_rtsp_url: 'rtsp://192.0.2.77:8554/live/main',
			sub_rtsp_url: 'rtsp://192.0.2.77:8554/live/sub'
		}
	});
});

test('updates server and storage settings before applying a restart', async ({ page }) => {
	const targetOrigin = 'http://localhost:3200';
	let targetMetricsChecks = 0;
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
				host: 'localhost',
				port: 3200,
				camera_count: 0,
				storage: updatedStorage,
				recording_estimate: recordingEstimate
			},
			restart_required: true
		}
	});
	await page.route(`${targetOrigin}/metrics`, async (route) => {
		targetMetricsChecks += 1;
		await route.fulfill({
			contentType: 'text/plain',
			body: '# EOF\n'
		});
	});
	await page.route(`${targetOrigin}/settings`, async (route) => {
		await route.fulfill({ contentType: 'text/html', body: '<main>Restarted settings</main>' });
	});

	await page.goto('/settings');
	await page.getByRole('button', { name: 'Edit storage' }).click();
	await page.getByLabel('Host').fill('localhost');
	await page.getByLabel('Port').fill('3200');
	await page.getByLabel('Medium-term path').fill('/archive/medium');
	await page.getByLabel('Long-term path').fill('/archive/long');
	await page.getByLabel('Recording metadata database path').fill('/archive/metadata/recordings.db');
	await page.getByLabel('Event JPEG storage path').fill('/archive/events');
	await page.getByLabel('Event JPEG limit MB').fill('512');
	await page.getByLabel('Move current storage files').check();
	await page.getByLabel('Short-term buffer seconds').fill('30');
	await page.getByLabel('Medium-term segment seconds').fill('120');
	await page.getByLabel('Flush interval seconds').fill('15');
	await page.getByLabel('Write buffer bytes').fill('16384');
	await page.getByLabel('Long-term max GB').fill('24');
	await page.getByRole('button', { name: 'Save settings' }).click();

	await expect(page.getByText('Server and storage settings saved.', { exact: true })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Apply changes' })).toBeVisible();
	await expect(page.getByText('localhost', { exact: true })).toBeVisible();
	await expect(page.getByText('3200', { exact: true })).toBeVisible();
	await expect(page.getByText('30s', { exact: true })).toBeVisible();
	const runtimeSettings = page.locator('#runtime-settings-form');
	await expect(
		runtimeSettings.getByText('/archive/metadata/recordings.db', { exact: true })
	).toBeVisible();
	await expect(runtimeSettings.getByText('/archive/events', { exact: true })).toBeVisible();
	await expect(page.getByText('Current recording estimate', { exact: true })).toBeVisible();
	expect(controls.runtimeUpdates).toEqual([
		{
			host: 'localhost',
			port: 3200,
			storage: updatedStorage,
			move_existing_recordings: true
		}
	]);
	await page.getByRole('button', { name: 'Apply changes' }).click();

	await expect(page).toHaveURL(`${targetOrigin}/settings`);
	await expect(page.getByText('Restarted settings', { exact: true })).toBeVisible();
	expect(targetMetricsChecks).toBeGreaterThan(0);
	expect(controls.restarts).toBe(1);
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
	await page.getByRole('button', { name: 'Save settings' }).click();
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
	await page.getByRole('button', { name: 'Save settings' }).click();

	const applying = page.locator('[data-settings-applying]');
	await expect(applying).toContainText('Applying server and storage settings');
	await expect(applying).toContainText('Confirmed values remain visible');
	await expect(port).toHaveValue('3000');
	await expect(port).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Cancel' }).last()).toBeDisabled();
	expect(controls.runtimeUpdates).toHaveLength(1);

	releaseUpdate();
	await expect(applying).toHaveCount(0);
	await expect(page.getByText('Server and storage settings saved.', { exact: true })).toBeVisible();
	await expect(page.getByText('3201', { exact: true })).toBeVisible();
});
