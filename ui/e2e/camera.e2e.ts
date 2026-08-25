import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import type {
	CameraCatalogCamera,
	CameraHealth,
	CameraListItem,
	CameraSettings
} from '../src/lib/types';

const CCTV_DATABASE_CAMERA_URL = 'https://www.cctv-database.com/camera/annke-fcd800-i91et/';

function catalogReference(camera: CameraListItem): CameraCatalogCamera {
	return {
		id: 'test-camera-rtsp',
		brand: camera.manufacturer ?? 'Test Camera',
		model: camera.model ?? 'RTSP Test Camera',
		aliases: [],
		camera_type: 'camera',
		resolution_label: null,
		megapixels: null,
		sensor: null,
		field_of_view: null,
		night_vision: null,
		ip_rating: null,
		ik_rating: null,
		two_way_audio: null,
		release_year: null,
		community_notes_count: 0,
		protocols: ['onvif', 'rtsp'],
		codecs: ['h264'],
		streams: [],
		sources: [CCTV_DATABASE_CAMERA_URL],
		stream_hints: null
	};
}

function streamHealth(id: string, backend: string, transport: string): CameraHealth {
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

function mobilePtzCamera(): { camera: CameraListItem; health: CameraHealth } {
	const health = streamHealth('fake-reo-proto', 'reo-proto', 'tcp');
	return {
		health,
		camera: {
			id: 'fake-reo-proto',
			ip: health.ip,
			name: 'Front Door',
			manufacturer: 'Reolink',
			model: 'RLC-Test',
			firmware_version: 'test-camera',
			serial_number: 'REO-0001',
			hardware_id: 'reo-test',
			hostname: 'front-door',
			mac_address: '02:00:00:00:00:42',
			is_reolink: true,
			backend: 'reo-proto',
			transport: 'tcp',
			web_url: 'http://192.0.2.42',
			ports: { http: 80, https: null, rtsp: 554, onvif: 8000 },
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
					framerate: 15,
					bitrate_kbps: 18_400,
					gop: 30,
					h264_profile: null,
					audio: { encoding: 'aac', sample_rate: 16_000, bitrate_kbps: 64 }
				},
				{
					name: 'subStream',
					stream: 'sub',
					encoding: 'h264',
					resolution: '640x360',
					framerate: 15,
					bitrate_kbps: 600,
					gop: 30,
					h264_profile: 'baseline',
					audio: null
				}
			]
		}
	};
}

function configuredCamera(camera: CameraListItem): CameraSettings {
	return {
		id: camera.id,
		ip: camera.ip,
		display_name: camera.name,
		manufacturer_override: null,
		username_configured: true,
		password_configured: true,
		onvif_port: camera.ports?.onvif ?? null,
		http_port: camera.ports?.http ?? null,
		main_rtsp_url: `rtsp://${camera.ip}:554/main`,
		sub_rtsp_url: `rtsp://${camera.ip}:554/sub`,
		uid_configured: true,
		backend: camera.backend === 'retina' ? 'retina' : 'reo-proto',
		transport: camera.transport === 'udp' ? 'udp' : 'tcp',
		record_generic_motion_events: false,
		recording_mode: 'event-boost',
		event_recording_duration_secs: 60,
		health: 'online',
		model: camera.model
	};
}

test('shows fake Retina UDP configuration and live stream observations', async ({ page }) => {
	const health = streamHealth('fake-retina', 'retina', 'udp');
	const camera: CameraListItem = {
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
	};
	await mockControlPeer(page, {
		cameras: [camera],
		health: { cameras: [{ ...health, configured_profiles: camera.profiles }] },
		cameraCatalogSearchResults: [catalogReference(camera)],
		motionDetection: { supported: false, controllable: false, enabled: null, error: null }
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
	await expect(page.getByRole('heading', { name: 'PTZ', exact: true })).toHaveCount(0);
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
	await expect(
		page.getByRole('link', { name: 'Open Test Camera RTSP Test Camera on CCTV Database' })
	).toHaveAttribute('href', CCTV_DATABASE_CAMERA_URL);

	await page.setViewportSize({ width: 390, height: 844 });
	await expect(
		page.getByRole('link', { name: 'Open Test Camera RTSP Test Camera on CCTV Database' })
	).toHaveAttribute('href', CCTV_DATABASE_CAMERA_URL);
});

test('saves and restores a camera manufacturer override', async ({ page }) => {
	const health = { ...streamHealth('fake-retina', 'retina', 'tcp'), manufacturer: 'ONVIF' };
	const camera = (): CameraListItem => ({
		id: 'fake-retina',
		ip: health.ip,
		name: health.name,
		manufacturer: 'ONVIF',
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
	const controls = await mockControlPeer(page, {
		cameras: [camera()],
		health: { cameras: [health] },
		motionDetection: { supported: false, controllable: false, enabled: null, error: null }
	});
	await page.goto('/camera?camera=fake-retina');
	await page.getByRole('button', { name: 'Edit manufacturer' }).click();
	await page.getByRole('textbox', { name: 'Manufacturer' }).fill('Hikvision');
	await page.getByRole('button', { name: 'Save manufacturer' }).click();
	await expect(page.getByText('Hikvision', { exact: true })).toBeVisible();

	await page.getByRole('button', { name: 'Use camera-reported manufacturer' }).click();
	await expect(page.getByText('ONVIF', { exact: true })).toBeVisible();
	await expect(controls.manufacturer).toEqual([
		{ sourceId: 'fake-retina', manufacturer: 'Hikvision' },
		{ sourceId: 'fake-retina', manufacturer: null }
	]);
});

test('edits this camera configuration without replacing untouched credentials', async ({
	page
}) => {
	const { camera, health } = mobilePtzCamera();
	const settings = configuredCamera(camera);
	const updatedSettings = {
		...settings,
		display_name: 'Front entrance',
		transport: 'udp' as const,
		recording_mode: 'main' as const
	};
	const controls = await mockControlPeer(page, {
		cameras: [camera],
		health: { cameras: [{ ...health, configured_profiles: camera.profiles }] },
		cameraSettings: [settings],
		cameraUpdateResult: { camera: updatedSettings, restart_required: true },
		motionDetection: { supported: true, controllable: true, enabled: true, error: null }
	});
	await page.goto(`/camera?camera=${camera.id}`);

	await page.getByRole('button', { name: 'Edit settings' }).click();
	const editor = page.locator('[data-camera-configuration-editor]');
	await expect(editor.getByRole('heading', { name: 'Edit camera settings' })).toBeVisible();
	await expect(editor.getByLabel('Username')).toHaveAttribute(
		'placeholder',
		'Configured · enter to replace'
	);
	await editor.getByLabel('Display name').fill('Front entrance');
	await editor.getByLabel('Transport').selectOption('udp');
	await editor.getByLabel('Recording mode').selectOption('main');
	await editor.getByRole('button', { name: 'Save camera settings' }).click();

	await expect(page.getByText('Camera settings saved. Restart KeepPeek')).toBeVisible();
	await expect(editor).toHaveCount(0);
	await expect.poll(() => controls.cameraUpdates).toHaveLength(1);
	const submitted = controls.cameraUpdates[0];
	expect(submitted?.ip).toBe(camera.ip);
	expect(submitted?.update).toMatchObject({
		display_name: 'Front entrance',
		transport: 'udp',
		recording_mode: 'main',
		record_generic_motion_events: false
	});
	expect(submitted?.update).not.toHaveProperty('username');
	expect(submitted?.update).not.toHaveProperty('password');
});

test('Board 7 operates Reo-Proto PTZ over WebRTC and updates its motion setting', async ({
	page
}) => {
	const health = streamHealth('fake-reo-proto', 'reo-proto', 'tcp');
	const motion = { supported: true, controllable: true, enabled: true, error: null };
	const camera: CameraListItem = {
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
	};
	const controls = await mockControlPeer(page, {
		cameras: [camera],
		health: { cameras: [{ ...health, configured_profiles: camera.profiles }] },
		motionDetection: motion,
		ptzPresets: [{ id: 7, name: 'Gate' }]
	});
	await page.goto('/camera?camera=fake-reo-proto');

	await expect(page.getByRole('heading', { name: 'Fake Reo-Proto' })).toBeVisible();
	await expect(page.getByText('reo-proto', { exact: true })).toBeVisible();
	await expect(page.getByText('tcp', { exact: true })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'PTZ', exact: true })).toBeVisible();
	for (const name of [
		'Tilt up',
		'Pan left',
		'Stop PTZ',
		'Pan right',
		'Tilt down',
		'Zoom out',
		'Zoom in'
	]) {
		await expect(page.getByRole('button', { name })).toBeEnabled();
	}
	const gatePreset = page.getByRole('button', { name: 'Gate', exact: true });
	await expect(gatePreset).toBeVisible();
	const panRight = page.getByRole('button', { name: 'Pan right' });
	await panRight.hover();
	await page.mouse.down();
	await expect
		.poll(() => controls.ptz)
		.toContainEqual({
			sourceId: 'fake-reo-proto',
			action: 'continuous',
			pan: 1,
			tilt: 0,
			zoom: 0
		});
	await page.mouse.up();
	await expect
		.poll(() => controls.ptz.filter((request) => request.action === 'stop'))
		.toHaveLength(1);
	await gatePreset.click();
	await expect
		.poll(() => controls.ptz)
		.toContainEqual({ sourceId: 'fake-reo-proto', action: 'gotoPreset', presetId: 7 });
	const tiltUp = page.getByRole('button', { name: 'Tilt up' });
	await tiltUp.press('Space');
	await expect
		.poll(() => controls.ptz)
		.toContainEqual({
			sourceId: 'fake-reo-proto',
			action: 'continuous',
			pan: 0,
			tilt: 1,
			zoom: 0
		});
	await expect
		.poll(() => controls.ptz.filter((request) => request.action === 'stop'))
		.toHaveLength(2);
	const motionSwitch = page.locator('input[role="switch"]');
	await expect(motionSwitch).toBeChecked();
	await motionSwitch.uncheck();
	await expect
		.poll(() => controls.motion)
		.toEqual([{ sourceId: 'fake-reo-proto', enabled: false }]);
	await expect(motionSwitch).not.toBeChecked();
	await expect(page.getByText('Disabled', { exact: true })).toBeVisible();
	await page.setViewportSize({ width: 390, height: 844 });
	await page.getByRole('button', { name: 'PTZ', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Fake Reo-Proto · PTZ' })).toBeVisible();
	await expect(panRight).toBeEnabled();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

const liveCameraId = '127.0.0.1';
const skipsH264DecodeOnWindowsCi = process.platform === 'win32' && Boolean(process.env.CI);

test('renders native Camera preview with fail-closed PTZ evidence', async ({ page }) => {
	test.skip(
		skipsH264DecodeOnWindowsCi,
		'Windows CI browsers do not expose decoded H.264 frames for this WebRTC stream.'
	);
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (cause) => browserErrors.push(cause.message));
	let ptzRequests = 0;
	page.on('request', (request) => {
		if (new URL(request.url()).pathname.toLocaleLowerCase().startsWith('/ptz')) ptzRequests += 1;
	});
	await page.goto(`/camera?camera=${liveCameraId}`);

	await expect(page.getByRole('navigation', { name: 'Camera sections' })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Live preview' })).toBeVisible();
	const liveView = page.locator(`[data-camera-id="${liveCameraId}"]`);
	await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
	await expect(liveView.locator('canvas')).toHaveCount(0);
	const video = liveView.locator('video');
	await expect(video).toBeVisible();
	await expect
		.poll(
			() =>
				video.evaluate((element) => {
					const media = element as HTMLVideoElement;
					return `${media.videoWidth}x${media.videoHeight}:${media.getVideoPlaybackQuality().totalVideoFrames}`;
				}),
			{ timeout: 30_000 }
		)
		.toMatch(/^640x360:[1-9]\d*$/);

	await expect(page.getByRole('heading', { name: 'PTZ', exact: true })).toHaveCount(0);
	await page.getByRole('button', { name: 'Test connection' }).click();
	await expect(page.getByText(/^Connection verified · /)).toBeVisible();
	expect(ptzRequests).toBe(0);
	expect(browserErrors).toEqual([]);
});

test('keeps Camera preview and PTZ evidence usable at the authored mobile viewport', async ({
	page
}) => {
	test.skip(
		skipsH264DecodeOnWindowsCi,
		'Windows CI browsers do not expose decoded H.264 frames for this WebRTC stream.'
	);
	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto(`/camera?camera=${liveCameraId}`);

	await expect(page.getByRole('navigation', { name: 'Camera sections' })).toBeHidden();
	await expect(page.locator(`[data-camera-id="${liveCameraId}"]`)).toHaveAttribute(
		'data-status',
		'live',
		{ timeout: 30_000 }
	);
	await expect(page.getByRole('heading', { name: 'PTZ', exact: true })).toHaveCount(0);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('renders Board 24 mobile live camera without fabricated recent events or audio controls', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const { camera, health } = mobilePtzCamera();
	await mockControlPeer(page, {
		cameras: [camera],
		health: {
			cameras: [{ ...health, name: camera.name ?? camera.id, configured_profiles: camera.profiles }]
		},
		motionDetection: { supported: true, controllable: true, enabled: true, error: null },
		ptzPresets: [{ id: 7, name: 'Front step' }]
	});

	await page.goto('/camera?camera=fake-reo-proto');

	const mobile = page.locator('[data-mobile-camera-page="live"]');
	await expect(mobile).toBeVisible();
	await expect(mobile).toContainText('Recent event evidence unavailable');
	await expect(mobile.getByRole('button', { name: 'PTZ', exact: true })).toBeEnabled();
	await expect(mobile.getByRole('button', { name: 'Talk', exact: true })).toBeDisabled();
	await expect(mobile.getByRole('button', { name: 'Listen', exact: true })).toBeDisabled();
	await expect(page.locator('[data-shell-mobile-nav]')).toHaveCount(0);
});

test('renders Board 24 mobile PTZ through the shared WebRTC control owner', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const { camera, health } = mobilePtzCamera();
	const controls = await mockControlPeer(page, {
		cameras: [camera],
		health: {
			cameras: [{ ...health, name: camera.name ?? camera.id, configured_profiles: camera.profiles }]
		},
		motionDetection: { supported: true, controllable: true, enabled: true, error: null },
		ptzPresets: [{ id: 7, name: 'Front step' }]
	});
	await page.goto('/camera?camera=fake-reo-proto');
	await page.getByRole('button', { name: 'PTZ', exact: true }).click();

	const mobile = page.locator('[data-mobile-camera-page="ptz"]');
	const panRight = mobile.getByRole('button', { name: 'Pan right' });
	await panRight.hover();
	await page.mouse.down();
	await expect
		.poll(() => controls.ptz)
		.toContainEqual({
			sourceId: 'fake-reo-proto',
			action: 'continuous',
			pan: 1,
			tilt: 0,
			zoom: 0
		});
	await page.mouse.up();
	await mobile.getByRole('button', { name: 'Front step', exact: true }).click();
	await expect
		.poll(() => controls.ptz)
		.toContainEqual({ sourceId: 'fake-reo-proto', action: 'gotoPreset', presetId: 7 });
});

test('renders Board 24 mobile settings as the editable per-camera owner', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const { camera, health } = mobilePtzCamera();
	const settings = configuredCamera(camera);
	const controls = await mockControlPeer(page, {
		cameras: [camera],
		cameraSettings: [settings],
		health: {
			cameras: [{ ...health, name: camera.name ?? camera.id, configured_profiles: camera.profiles }]
		},
		motionDetection: { supported: true, controllable: true, enabled: true, error: null }
	});
	await page.goto('/camera?camera=fake-reo-proto');
	await page.getByRole('button', { name: 'Settings', exact: true }).click();

	const editor = page.locator('[data-camera-configuration-editor]');
	await expect(editor.getByRole('heading', { name: 'Edit camera settings' })).toBeVisible();
	await expect(editor.getByLabel('Display name')).toHaveValue('Front Door');
	await expect(editor.getByLabel('Username')).toHaveAttribute(
		'placeholder',
		'Configured · enter to replace'
	);
	await expect(editor.getByLabel('Recording mode')).toHaveValue('event-boost');
	await expect(editor.getByRole('button', { name: 'Save camera settings' })).toBeEnabled();
	expect(controls.cameraUpdates).toEqual([]);
});
