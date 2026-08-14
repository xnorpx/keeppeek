import { expect, test } from '@playwright/test';

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

test('discovers and configures a camera without rendering its saved password', async ({ page }) => {
	const writes: unknown[] = [];
	await page.route('**/api/config', async (route) => {
		await route.fulfill({
			json: {
				host: '0.0.0.0',
				port: 3000,
				camera_count: 1,
				storage,
				recording_estimate: recordingEstimate
			}
		});
	});
	await page.route('**/api/settings/cameras', async (route) => {
		await route.fulfill({
			json: [
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
			]
		});
	});
	await page.route('**/api/settings/cameras/discover', async (route) => {
		expect(route.request().postDataJSON()).toEqual({ subnets: [137, 138] });
		await route.fulfill({
			json: [
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
			]
		});
	});
	await page.route('**/api/settings/cameras/192.0.2.77', async (route) => {
		expect(route.request().method()).toBe('PUT');
		const update = route.request().postDataJSON();
		writes.push(update);
		expect(update).toMatchObject({
			display_name: 'Front Gate',
			username: 'operator',
			password: 'write-only-password',
			backend: 'reo-proto',
			transport: 'tcp',
			main_rtsp_url: 'rtsp://192.0.2.77:8554/live/main',
			sub_rtsp_url: 'rtsp://192.0.2.77:8554/live/sub'
		});
		await route.fulfill({
			json: {
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
	});

	await page.goto('/settings');

	await expect(page.getByRole('heading', { name: 'Settings' })).toBeVisible();
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
	expect(writes).toHaveLength(1);
});

test('updates server and storage settings before applying a restart', async ({ page }) => {
	const targetOrigin = 'http://localhost:3200';
	let targetHealthChecks = 0;
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
	await page.route('**/api/config', async (route) => {
		await route.fulfill({
			json: {
				host: '0.0.0.0',
				port: 3000,
				camera_count: 0,
				storage,
				recording_estimate: recordingEstimate
			}
		});
	});
	await page.route('**/api/settings/cameras', async (route) => {
		await route.fulfill({ json: [] });
	});
	await page.route('**/api/settings/config', async (route) => {
		expect(route.request().method()).toBe('PUT');
		expect(route.request().postDataJSON()).toEqual({
			host: 'localhost',
			port: 3200,
			storage: updatedStorage,
			move_existing_recordings: true
		});
		await route.fulfill({
			json: {
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
	});
	await page.route('**/api/settings/restart', async (route) => {
		expect(route.request().method()).toBe('POST');
		await route.fulfill({ json: { restarting: true } });
	});
	await page.route(`${targetOrigin}/health`, async (route) => {
		targetHealthChecks += 1;
		await route.fulfill({
			json: { status: 'ok' },
			headers: { 'Access-Control-Allow-Origin': '*' }
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
	await expect(page.getByText('/archive/metadata/recordings.db', { exact: true })).toBeVisible();
	await expect(page.getByText('/archive/events', { exact: true })).toBeVisible();
	await expect(page.getByText('Current recording estimate', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Apply changes' }).click();

	await expect(page).toHaveURL(`${targetOrigin}/settings`);
	await expect(page.getByText('Restarted settings', { exact: true })).toBeVisible();
	expect(targetHealthChecks).toBeGreaterThan(0);
});

test('uses the browser hostname after a wildcard host changes port', async ({ page }) => {
	const targetOrigin = 'http://127.0.0.1:3201';
	await page.route('**/api/config', async (route) => {
		await route.fulfill({
			json: {
				host: '0.0.0.0',
				port: 3000,
				camera_count: 0,
				storage,
				recording_estimate: recordingEstimate
			}
		});
	});
	await page.route('**/api/settings/cameras', async (route) => {
		await route.fulfill({ json: [] });
	});
	await page.route('**/api/settings/config', async (route) => {
		expect(route.request().postDataJSON()).toEqual({
			host: '0.0.0.0',
			port: 3201,
			storage,
			move_existing_recordings: false
		});
		await route.fulfill({
			json: {
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
	});
	await page.route('**/api/settings/restart', async (route) => {
		await route.fulfill({ json: { restarting: true } });
	});
	await page.route(`${targetOrigin}/health`, async (route) => {
		await route.fulfill({
			json: { status: 'ok' },
			headers: { 'Access-Control-Allow-Origin': '*' }
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
});
