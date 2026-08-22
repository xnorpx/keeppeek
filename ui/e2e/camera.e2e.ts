import { expect, test } from '@playwright/test';

const homeKitSetupQrSvgBase64 =
	'PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIxNjAiIGhlaWdodD0iMTYwIi8+';

test.beforeEach(async ({ page }) => {
	await page.route('**/api/settings/homekit', async (route) => {
		await route.fulfill({
			json: {
				enabled: false,
				name: 'KeepPeek',
				bind: '0.0.0.0',
				port: 32000,
				exported_camera_count: 0,
				accessories: []
			}
		});
	});
});

function streamHealth(id: string, backend: string, transport: string) {
	return {
		id,
		ip: id === 'fake-retina' ? '192.0.2.41' : '192.0.2.42',
		name: id === 'fake-retina' ? 'Fake Retina' : 'Fake Reo-Proto',
		manufacturer: id === 'fake-retina' ? 'Test Camera' : 'Reolink',
		model: id === 'fake-retina' ? 'RTSP Test Camera' : 'RLC-Test',
		firmware_version: 'test-camera',
		backend,
		transport,
		state: 'online',
		lifecycle: 'connected',
		last_error: null,
		configured_profiles: [],
		streams: [
			{
				type: 'video_main',
				codec: id === 'fake-retina' ? 'h264' : 'h265',
				resolution: id === 'fake-retina' ? '1920x1080' : '3840x2160',
				fps: 25,
				expected_fps: 25,
				kf_fps: 1,
				kbps: 4_000,
				max_frame_kb: 512,
				frames: 12_500,
				bytes: 2_000_000_000,
				keyframes: 500,
				gap_avg_ms: 40,
				jitter_p99_ms: 6,
				drops: 0,
				errors: 0,
				reconnects: 2,
				updated_at_ms: Date.now(),
				report_age_ms: 120
			},
			{
				type: 'audio',
				codec: 'aac',
				fps: 15.6,
				kbps: 64,
				max_frame_kb: 0.4,
				frames: 156,
				bytes: 64_000,
				updated_at_ms: Date.now(),
				report_age_ms: 120
			}
		]
	};
}

test('shows fake Retina UDP configuration and live stream observations', async ({ page }) => {
	const health = streamHealth('fake-retina', 'retina', 'udp');
	await page.route('**/api/cameras/fake-retina/details', async (route) => {
		await route.fulfill({
			json: {
				camera: {
					id: 'fake-retina',
					ip: health.ip,
					name: health.name,
					manufacturer: health.manufacturer,
					model: health.model,
					firmware_version: health.firmware_version,
					serial_number: 'RETINA-0001',
					hardware_id: 'rtsp-test',
					hostname: 'fake-retina',
					mac_address: '02:00:00:00:00:41',
					is_reolink: false,
					backend: 'retina',
					transport: 'udp',
					web_url: 'http://192.0.2.41',
					ports: { http: 80, https: null, rtsp: 554, onvif: 8000 },
					capabilities: {
						ptz: false,
						audio: true,
						events: false,
						recording: false,
						analytics: false,
						imaging: false,
						two_way_audio: false
					},
					profiles: [
						{
							name: 'Main',
							stream: 'main',
							encoding: 'h264',
							resolution: '1920x1080',
							framerate: 25,
							bitrate_kbps: 4096,
							gop: 25,
							h264_profile: 'baseline',
							audio: { encoding: 'aac', sample_rate: 48_000, bitrate_kbps: 128 }
						}
					]
				},
				health,
				motion_detection: { supported: false, controllable: false, enabled: null, error: null }
			}
		});
	});
	await page.route('**/api/health', async (route) => {
		await route.fulfill({ json: { cameras: [health] } });
	});

	await page.goto('/camera?camera=fake-retina');

	await expect(page.getByRole('heading', { name: 'Fake Retina' })).toBeVisible();
	await expect(page.getByText('retina', { exact: true })).toBeVisible();
	await expect(page.getByText('udp', { exact: true })).toBeVisible();
	await expect(page.getByText('Known service ports', { exact: true })).toBeVisible();
	await expect(page.getByText('HTTP 80 · RTSP 554 · ONVIF 8000', { exact: true })).toBeVisible();
	await expect(page.getByText('Main video', { exact: true })).toBeVisible();
	await expect(page.getByText('1920x1080', { exact: true })).toBeVisible();
	await expect(page.getByText('Profile baseline', { exact: true })).toBeVisible();
	await expect(page.getByText('512 kB', { exact: true })).toBeVisible();
	await expect(page.getByText('2', { exact: true })).toBeVisible();
	const audioRow = page.getByRole('row', { name: /Audio aac.*15\.6.*400 B.*N\/A/ });
	await expect(audioRow).toBeVisible();
	await expect(
		page.getByText('The camera UI link works only from a device on the same network as the camera.')
	).toBeVisible();
	await expect(page.getByRole('link', { name: 'Open camera UI' })).toHaveAttribute(
		'href',
		'http://192.0.2.41'
	);
});

test('owns HomeKit pairing and resets a stale stored controller', async ({ page }) => {
	const health = streamHealth('fake-retina', 'retina', 'tcp');
	let paired = true;
	let resetRequests = 0;
	await page.route('**/api/cameras/fake-retina/details', async (route) => {
		await route.fulfill({
			json: {
				camera: {
					id: 'fake-retina',
					ip: health.ip,
					name: health.name,
					manufacturer: health.manufacturer,
					model: health.model,
					firmware_version: health.firmware_version,
					serial_number: 'RETINA-0001',
					hardware_id: 'rtsp-test',
					hostname: 'fake-retina',
					mac_address: '02:00:00:00:00:41',
					is_reolink: false,
					backend: 'retina',
					transport: 'tcp',
					web_url: 'http://192.0.2.41',
					ports: { http: 80, https: null, rtsp: 554, onvif: 8000 },
					capabilities: {},
					profiles: []
				},
				health,
				motion_detection: { supported: false, controllable: false, enabled: null, error: null }
			}
		});
	});
	await page.unroute('**/api/settings/homekit');
	await page.route('**/api/settings/homekit', async (route) => {
		await route.fulfill({
			json: {
				enabled: true,
				name: 'KeepPeek',
				bind: '0.0.0.0',
				port: 32000,
				exported_camera_count: 1,
				accessories: [
					{
						camera_id: health.ip,
						name: health.name,
						paired,
						pairing_count: paired ? 1 : 0,
						port: 32000,
						setup_code: paired ? null : '123-45-678',
						setup_qr_svg_base64: paired ? null : homeKitSetupQrSvgBase64
					}
				]
			}
		});
	});
	await page.route('**/api/cameras/fake-retina/homekit/pairings', async (route) => {
		expect(route.request().method()).toBe('DELETE');
		resetRequests += 1;
		paired = false;
		await route.fulfill({ status: 204 });
	});
	await page.route('**/api/settings/restart', async (route) => {
		await route.fulfill({ json: { restarting: true } });
	});
	await page.route(
		(url) => url.pathname === '/health',
		async (route) => {
			await route.fulfill({ json: { status: 'ok' } });
		}
	);
	await page.route('**/api/health', async (route) => {
		await route.fulfill({ json: { cameras: [health] } });
	});
	page.on('dialog', (dialog) => void dialog.accept());

	await page.goto('/camera?camera=fake-retina');

	await expect(page.getByRole('heading', { name: 'HomeKit' })).toBeVisible();
	await expect(page.getByText('1 controller key stored', { exact: true })).toBeVisible();
	await expect(page.getByRole('img', { name: 'Fake Retina HomeKit setup QR code' })).toHaveCount(0);
	await page.getByRole('button', { name: 'Reset stored pairing' }).click();

	await expect.poll(() => resetRequests).toBe(1);
	await expect(page.getByText('Ready to pair', { exact: true })).toBeVisible();
	await expect(page.getByRole('img', { name: 'Fake Retina HomeKit setup QR code' })).toBeVisible();
	await expect(page.getByText('123-45-678', { exact: true })).toBeVisible();
});

test('saves and restores a camera manufacturer override', async ({ page }) => {
	const health = { ...streamHealth('fake-retina', 'retina', 'tcp'), manufacturer: 'ONVIF' };
	let manufacturer = 'ONVIF';
	const updates: Array<string | null> = [];
	const camera = () => ({
		id: 'fake-retina',
		ip: health.ip,
		name: health.name,
		manufacturer,
		model: health.model,
		firmware_version: health.firmware_version,
		serial_number: 'RETINA-0001',
		hardware_id: 'rtsp-test',
		hostname: 'fake-retina',
		mac_address: '02:00:00:00:00:41',
		is_reolink: false,
		backend: 'retina',
		transport: 'tcp',
		web_url: 'http://192.0.2.41',
		ports: { http: 80, https: null, rtsp: 554, onvif: 8000 },
		capabilities: {
			ptz: false,
			audio: true,
			events: false,
			recording: false,
			analytics: false,
			imaging: false,
			two_way_audio: false
		},
		profiles: []
	});

	await page.route('**/api/cameras/fake-retina/details', async (route) => {
		await route.fulfill({
			json: {
				camera: camera(),
				health,
				motion_detection: { supported: false, controllable: false, enabled: null, error: null }
			}
		});
	});
	await page.route('**/api/cameras/fake-retina/manufacturer', async (route) => {
		expect(route.request().method()).toBe('PUT');
		const update = route.request().postDataJSON() as { manufacturer: string | null };
		updates.push(update.manufacturer);
		manufacturer = update.manufacturer ?? 'ONVIF';
		await route.fulfill({ json: camera() });
	});
	await page.route('**/api/health', async (route) => {
		await route.fulfill({ json: { cameras: [health] } });
	});

	await page.goto('/camera?camera=fake-retina');
	await page.getByRole('button', { name: 'Edit manufacturer' }).click();
	await page.getByRole('textbox', { name: 'Manufacturer' }).fill('Hikvision');
	await page.getByRole('button', { name: 'Save manufacturer' }).click();
	await expect(page.getByText('Hikvision', { exact: true })).toBeVisible();

	await page.getByRole('button', { name: 'Use camera-reported manufacturer' }).click();
	await expect(page.getByText('ONVIF', { exact: true })).toBeVisible();
	await expect(updates).toEqual(['Hikvision', null]);
});

test('shows fake Reo-Proto TCP data and updates its motion setting', async ({ page }) => {
	const health = streamHealth('fake-reo-proto', 'reo-proto', 'tcp');
	let motion = { supported: true, controllable: true, enabled: true, error: null };
	const updates: boolean[] = [];
	await page.route('**/api/cameras/fake-reo-proto/details', async (route) => {
		await route.fulfill({
			json: {
				camera: {
					id: 'fake-reo-proto',
					ip: health.ip,
					name: health.name,
					manufacturer: health.manufacturer,
					model: health.model,
					firmware_version: health.firmware_version,
					serial_number: 'REO-0001',
					hardware_id: 'reo-test',
					hostname: 'fake-reo-proto',
					mac_address: '02:00:00:00:00:42',
					is_reolink: true,
					backend: 'reo-proto',
					transport: 'tcp',
					web_url: 'http://192.0.2.42',
					ports: { http: 80, https: null, rtsp: null, onvif: 8000 },
					capabilities: {
						ptz: true,
						audio: true,
						events: true,
						recording: true,
						analytics: true,
						imaging: true,
						two_way_audio: true
					},
					profiles: [
						{
							name: 'mainStream',
							stream: 'main',
							encoding: 'h265',
							resolution: '3840x2160',
							framerate: 25,
							bitrate_kbps: 8192,
							gop: 50,
							h264_profile: null,
							audio: { encoding: 'aac', sample_rate: 16_000, bitrate_kbps: 64 }
						}
					]
				},
				health,
				motion_detection: motion
			}
		});
	});
	await page.route('**/api/cameras/fake-reo-proto/motion', async (route) => {
		const update = route.request().postDataJSON() as { enabled: boolean };
		updates.push(update.enabled);
		motion = { ...motion, enabled: update.enabled };
		await route.fulfill({ json: motion });
	});
	await page.route('**/api/health', async (route) => {
		await route.fulfill({ json: { cameras: [health] } });
	});

	await page.goto('/camera?camera=fake-reo-proto');

	await expect(page.getByRole('heading', { name: 'Fake Reo-Proto' })).toBeVisible();
	await expect(page.getByText('reo-proto', { exact: true })).toBeVisible();
	await expect(page.getByText('tcp', { exact: true })).toBeVisible();
	const motionSwitch = page.locator('input[role="switch"]');
	await expect(motionSwitch).toBeChecked();
	await motionSwitch.uncheck();
	await expect.poll(() => updates).toEqual([false]);
	await expect(motionSwitch).not.toBeChecked();
	await expect(page.getByText('Disabled', { exact: true })).toBeVisible();
});
