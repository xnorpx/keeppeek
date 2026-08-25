import { expect, test, type Page } from '@playwright/test';
import type { CameraStreamVerification } from '../src/lib/types';
import { mockControlPeer } from './fixtures/control-peer';

const discoveredCamera = {
	ip: '192.0.2.77',
	brand: 'reolink',
	name: 'Front Gate',
	model: 'RLC-Test',
	onvif_port: null,
	sources: ['onvif', 'baichuan'],
	configured: false,
	health: null
};

const catalogCamera = {
	id: 'reolink-rlc-811a',
	brand: 'Reolink',
	model: 'RLC-811A',
	aliases: ['RLC 811 A'],
	camera_type: 'bullet',
	resolution_label: '4K UHD',
	megapixels: 8,
	sensor: '1/2.8" CMOS',
	field_of_view: '105-31 horizontal',
	night_vision: 'hybrid',
	ip_rating: 'IP67',
	ik_rating: null,
	two_way_audio: true,
	release_year: 2021,
	community_notes_count: 0,
	protocols: ['onvif', 'rtsp'],
	codecs: ['H.265', 'H.264'],
	streams: [
		{ name: 'main', resolution: '3840x2160', fps: 25, codec: 'H.265' },
		{ name: 'sub', resolution: null, fps: 10, codec: 'H.264' }
	],
	sources: ['https://reolink.com/product/rlc-811a/'],
	stream_hints: {
		main_rtsp_url: 'rtsp://192.0.2.77:554/Preview_01_main',
		sub_rtsp_url: 'rtsp://192.0.2.77:554/Preview_01_sub'
	}
};

const savedCamera = {
	id: discoveredCamera.ip,
	ip: discoveredCamera.ip,
	display_name: discoveredCamera.name,
	manufacturer_override: null,
	username_configured: true,
	password_configured: true,
	onvif_port: 8000,
	http_port: 80,
	main_rtsp_url: 'rtsp://192.0.2.77:8554/live/main',
	sub_rtsp_url: 'rtsp://192.0.2.77:8554/live/sub',
	uid_configured: false,
	backend: 'reo-proto' as const,
	transport: 'tcp' as const,
	record_generic_motion_events: false,
	recording_mode: 'event-boost' as const,
	event_recording_duration_secs: 60,
	health: null,
	model: null
};

async function completeThroughReview(page: Page): Promise<void> {
	const desktopWizard = page.locator('[data-desktop-camera-wizard]');
	await page.getByRole('button', { name: /Front Gate/ }).click();
	await desktopWizard.getByLabel('Username').fill('operator');
	await desktopWizard.getByLabel('Password').fill('write-only-password');
	await expect(desktopWizard.locator('[data-onvif-probe-status]')).toBeVisible();
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByRole('heading', { name: 'Connection options' })).toBeVisible();
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(
		page.getByText('KeepPeek received authenticated video evidence.', { exact: true })
	).toBeVisible();
	await desktopWizard.getByLabel(/Recording stream/).fill('rtsp://192.0.2.77:8554/live/main');
	await desktopWizard.getByLabel(/Live stream/).fill('rtsp://192.0.2.77:8554/live/sub');
	await desktopWizard.getByRole('button', { name: 'Verify streams' }).click();
	await expect(
		desktopWizard.getByText('KeepPeek received authenticated video evidence.', { exact: true })
	).toBeVisible();
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(desktopWizard.getByLabel('Camera name')).toHaveValue('Front Gate');
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByRole('heading', { name: 'Review & save' })).toBeVisible();
}

async function reachMobileStreams(page: Page): Promise<void> {
	await page.goto('/cameras/new');
	const findStage = page.locator('[data-mobile-camera-wizard="find-connect"]');
	await findStage.getByRole('button', { name: 'Scan this network' }).click();
	await findStage.getByRole('button', { name: /Front Gate/ }).click();
	await findStage.getByLabel('Username').fill('operator');
	await findStage.getByLabel('Password').fill('write-only-password');
	await findStage.getByRole('button', { name: 'Connect' }).click();
	await expect(page.locator('[data-mobile-camera-wizard="streams"]')).toBeVisible();
}

test('renders exactly one accessible wizard owner at each responsive breakpoint', async ({
	page
}) => {
	await mockControlPeer(page);
	await page.goto('/cameras/new');
	await expect(page.getByLabel('Address or RTSP URL')).toHaveCount(1);
	await expect(page.getByLabel('Username')).toHaveCount(1);
	await expect(page.getByLabel('Password')).toHaveCount(1);

	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page.locator('[data-mobile-camera-wizard="find-connect"]')).toBeVisible();
	await expect(page.getByLabel('Address or RTSP URL')).toHaveCount(1);
	await expect(page.getByLabel('Username')).toHaveCount(1);
	await expect(page.getByLabel('Password')).toHaveCount(1);
});

test('Board 12 discovers and saves a camera only after the fifth-step review', async ({ page }) => {
	let releaseDiscovery!: () => void;
	const discoveryGate = new Promise<void>((resolve) => {
		releaseDiscovery = resolve;
	});
	const controls = await mockControlPeer(page, {
		discoveredCameras: [discoveredCamera],
		discoveryGate,
		cameraUpdateResult: { camera: savedCamera, restart_required: true }
	});

	await page.goto('/cameras/new');
	await expect(page.getByText('NOTHING SAVED UNTIL STEP 5')).toBeVisible();
	await page.getByRole('button', { name: 'Discover cameras' }).click();
	await expect(page.getByRole('button', { name: 'Scanning network' })).toBeDisabled();
	const progress = page.getByRole('status', { name: 'Camera discovery progress' });
	await expect(progress).toContainText('0 devices answered so far');
	await expect(progress).toContainText('1 /24 network');
	await expect(page.getByRole('progressbar', { name: 'Discovery time target' })).toHaveAttribute(
		'aria-valuemax',
		'5000'
	);
	releaseDiscovery();
	await expect(page.getByRole('button', { name: /Front Gate/ })).toBeVisible();
	await expect(progress).toHaveCount(0);
	expect(controls.discoveryNetworks).toEqual([['192.168.1.0/24']]);

	await completeThroughReview(page);
	expect(controls.cameraUpdates).toEqual([]);
	await expect(page.getByText('write-only-password', { exact: true })).toHaveCount(0);
	await page.getByRole('button', { name: 'Save camera' }).click();

	await expect(page.getByRole('region', { name: 'Camera saved' })).toBeVisible();
	await expect(page.getByText('RESTART REQUIRED', { exact: true })).toBeVisible();
	await expect(page.getByText('Saved to configuration. Restart KeepPeek')).toBeVisible();
	await expect(page.getByRole('link', { name: 'Open camera' })).toHaveAttribute(
		'href',
		'/camera?camera=192.0.2.77'
	);
	await expect(page.getByRole('link', { name: 'Restart KeepPeek' })).toHaveAttribute(
		'href',
		'/settings#appearance'
	);
	expect(controls.cameraUpdates).toEqual([
		{
			ip: '192.0.2.77',
			update: {
				display_name: 'Front Gate',
				username: 'operator',
				password: 'write-only-password',
				onvif_port: 8000,
				http_port: 80,
				main_rtsp_url: 'rtsp://192.0.2.77:8554/live/main',
				sub_rtsp_url: 'rtsp://192.0.2.77:8554/live/sub',
				uid: null,
				backend: 'reo-proto',
				transport: 'tcp',
				record_generic_motion_events: false,
				recording_mode: 'event-boost',
				event_recording_duration_secs: 60
			}
		}
	]);
	await expect(page.getByText('write-only-password', { exact: true })).toHaveCount(0);
});

test('streams partial discovery rows and cancels without discarding them', async ({ page }) => {
	let releaseDiscovery!: () => void;
	const discoveryGate = new Promise<void>((resolve) => {
		releaseDiscovery = resolve;
	});
	const controls = await mockControlPeer(page, {
		discoveryGate,
		discoveryPartialCameras: [discoveredCamera],
		discoveredCameras: [discoveredCamera]
	});

	await page.goto('/cameras/new');
	const wizard = page.locator('[data-desktop-camera-wizard]');
	await wizard.getByRole('button', { name: 'Discover cameras' }).click();
	await expect(wizard.getByRole('button', { name: /Front Gate/ })).toBeVisible();
	await expect(wizard.getByRole('status', { name: 'Camera discovery progress' })).toBeVisible();
	await expect.poll(() => controls.discoveryPolls).toBeGreaterThan(0);

	await wizard.getByRole('button', { name: 'Cancel discovery' }).click();
	await expect(
		wizard.getByText('Discovery cancelled. Cameras already found remain available.')
	).toBeVisible();
	await expect.poll(() => controls.discoveryCancelIds).toHaveLength(1);
	await wizard.getByRole('button', { name: /Front Gate/ }).click();
	await expect(wizard.getByText('Selected 192.0.2.77')).toBeVisible();
	expect(controls.cameraUpdates).toEqual([]);
	releaseDiscovery();
});

test('uses the preferred attached network and configured credential defaults without exposing them', async ({
	page
}) => {
	const controls = await mockControlPeer(page, {
		onboardingDefaults: {
			username_configured: true,
			password_configured: true,
			networks: [
				{ cidr: '192.168.137.0/24', interface_name: 'en0', preferred: true },
				{ cidr: '192.168.1.0/24', interface_name: 'bridge0', preferred: false }
			]
		},
		discoveredCameras: [discoveredCamera],
		cameraUpdateResult: {
			camera: {
				...savedCamera,
				main_rtsp_url: 'rtsp://192.0.2.77:554/onvif-main',
				sub_rtsp_url: 'rtsp://192.0.2.77:554/onvif-sub'
			},
			restart_required: true
		}
	});

	await page.goto('/cameras/new');
	const wizard = page.locator('[data-desktop-camera-wizard]');
	await expect(wizard.getByLabel('Subnet prefixes')).toHaveValue('192.168.137');
	await expect(
		wizard.getByRole('button', { name: /192\.168\.137\.0\/24.*ACTIVE/ })
	).toHaveAttribute('aria-pressed', 'true');
	await expect(wizard.getByRole('button', { name: /192\.168\.1\.0\/24/ })).toHaveAttribute(
		'aria-pressed',
		'false'
	);
	await wizard.getByRole('button', { name: 'Discover cameras' }).click();
	await wizard.getByRole('button', { name: /Front Gate/ }).click();
	await expect(wizard.getByLabel('Username')).toHaveValue('');
	await expect(wizard.getByLabel('Username')).toHaveAttribute('placeholder', 'Configured default');
	await expect(wizard.getByLabel('Password')).toHaveValue('');
	await expect(wizard.getByLabel('Password')).toHaveAttribute('placeholder', 'Configured default');
	await expect(wizard).toContainText(
		'Configured camera defaults are used without exposing their values.'
	);
	await expect(wizard.locator('[data-onvif-probe-status]')).toContainText(
		'ONVIF stream endpoints are ready on port 8000.'
	);

	await wizard.getByRole('button', { name: 'Continue' }).click();
	await wizard.getByRole('button', { name: 'Continue' }).click();
	await expect(wizard).toContainText('KeepPeek received authenticated video evidence.');
	await wizard.getByRole('button', { name: 'Continue' }).click();
	await wizard.getByRole('button', { name: 'Continue' }).click();
	await wizard.getByRole('button', { name: 'Save camera' }).click();

	await expect(page.getByRole('region', { name: 'Camera saved' })).toBeVisible();
	expect(controls.discoveryNetworks).toEqual([['192.168.137.0/24']]);
	expect(controls.cameraUpdates).toHaveLength(1);
	expect(controls.cameraUpdates[0]?.update).not.toHaveProperty('username');
	expect(controls.cameraUpdates[0]?.update).not.toHaveProperty('password');
	await expect(page.getByText('operator', { exact: true })).toHaveCount(0);
});

test('preserves a manually entered subnet when navigating back', async ({ page }) => {
	await mockControlPeer(page);
	await page.goto('/cameras/new');
	const wizard = page.locator('[data-desktop-camera-wizard]');
	await wizard.getByLabel('Subnet prefixes').fill('10.42.7');
	await wizard.getByLabel('Address or RTSP URL').fill('192.0.2.77');
	await wizard.getByLabel('Username').fill('operator');
	await wizard.getByLabel('Password').fill('write-only-password');
	await wizard.getByRole('button', { name: 'Continue' }).click();
	await expect(wizard.getByRole('heading', { name: 'Connection options' })).toBeVisible();

	await wizard.getByRole('button', { name: 'Back' }).click();

	await expect(wizard.getByLabel('Subnet prefixes')).toHaveValue('10.42.7');
});

test('blocks review and save until required media and keyframes are verified', async ({ page }) => {
	const failedStreams: CameraStreamVerification[] = [
		{
			stream: 'main',
			verified: false,
			codec: 'h264',
			resolution: '1920x1080',
			declared_fps: 25,
			frames_received: 4,
			keyframe_received: false,
			elapsed_ms: 500,
			error: 'No keyframe arrived.'
		},
		{
			stream: 'sub',
			verified: true,
			codec: 'h264',
			resolution: '640x360',
			declared_fps: 15,
			frames_received: 4,
			keyframe_received: true,
			elapsed_ms: 500,
			error: null
		}
	];
	const streamProbeResult = { streams: failedStreams };
	await mockControlPeer(page, { discoveredCameras: [discoveredCamera], streamProbeResult });
	await page.goto('/cameras/new');
	const wizard = page.locator('[data-desktop-camera-wizard]');
	await wizard.getByRole('button', { name: 'Discover cameras' }).click();
	await wizard.getByRole('button', { name: /Front Gate/ }).click();
	await wizard.getByLabel('Username').fill('operator');
	await wizard.getByLabel('Password').fill('write-only-password');
	await wizard.getByRole('button', { name: 'Continue' }).click();
	await wizard.getByRole('button', { name: 'Continue' }).click();

	await expect(wizard.getByText('No keyframe arrived.', { exact: true })).toBeVisible();
	await expect(wizard.getByText('Main · NOT VERIFIED', { exact: true })).toBeVisible();
	await expect(wizard.getByRole('button', { name: 'Continue' })).toBeDisabled();
	await expect(page.getByRole('heading', { name: 'Review & save' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Save camera' })).toHaveCount(0);

	streamProbeResult.streams = failedStreams.map((stream) => ({
		...stream,
		verified: true,
		keyframe_received: true,
		error: null
	}));
	await wizard.getByRole('button', { name: 'Verify streams' }).click();
	await expect(
		wizard.getByText('KeepPeek received authenticated video evidence.', { exact: true })
	).toBeVisible();
	await expect(wizard.getByRole('button', { name: 'Continue' })).toBeEnabled();
});

test('reports a dynamically started camera as online without asking for a restart', async ({
	page
}) => {
	const controls = await mockControlPeer(page, {
		discoveredCameras: [discoveredCamera],
		cameraUpdateResult: {
			camera: { ...savedCamera, health: 'healthy' },
			restart_required: false
		}
	});

	await page.goto('/cameras/new');
	await page.getByRole('button', { name: 'Discover cameras' }).click();
	await completeThroughReview(page);
	await page.getByRole('button', { name: 'Save camera' }).click();

	const saved = page.getByRole('region', { name: 'Camera saved' });
	await expect(saved.getByText('HEALTHY', { exact: true })).toBeVisible();
	await expect(saved).toContainText('Saved, started, and reporting online.');
	await expect(saved.getByRole('link', { name: 'Restart KeepPeek' })).toHaveCount(0);
	await expect(saved.getByRole('link', { name: 'Open diagnostics' })).toHaveCount(0);
	expect(controls.cameraUpdates).toHaveLength(1);
});

test('preserves the reviewed draft when the final config write is unavailable', async ({
	page
}) => {
	const controls = await mockControlPeer(page, {
		discoveredCameras: [discoveredCamera],
		cameraUpdateError: 'camera config is not writable'
	});

	await page.goto('/cameras/new');
	await page.getByRole('button', { name: 'Discover cameras' }).click();
	await completeThroughReview(page);
	await page.getByRole('button', { name: 'Save camera' }).click();

	await expect(page.getByRole('alert')).toContainText('camera config is not writable');
	await expect(page.getByRole('heading', { name: 'Review & save' })).toBeVisible();
	await expect(page.getByText('Front Gate', { exact: true })).toBeVisible();
	const review = page.locator('[data-desktop-camera-wizard]');
	await expect(review.getByText('event-boost', { exact: true })).toBeVisible();
	await expect(review.getByText('Video + keyframe verified', { exact: true })).toHaveCount(2);
	await expect(page.getByRole('button', { name: 'Save camera' })).toBeEnabled();
	expect(controls.cameraUpdates).toHaveLength(1);
});

test('uses catalog references when ONVIF does not report stream endpoints', async ({ page }) => {
	const controls = await mockControlPeer(page, {
		discoveredCameras: [{ ...discoveredCamera, catalog: catalogCamera }],
		streamProbeResult: { main_rtsp_url: null, sub_rtsp_url: null }
	});

	await page.goto('/cameras/new');
	await page.getByRole('button', { name: 'Discover cameras' }).click();
	await page.getByRole('button', { name: /Front Gate/ }).click();
	const desktopWizard = page.locator('[data-desktop-camera-wizard]');
	await expect(desktopWizard.getByText('Reolink RLC-811A', { exact: true })).toBeVisible();
	await expect(desktopWizard.getByText('MODEL REFERENCE', { exact: true })).toBeVisible();
	await expect(
		desktopWizard.getByText('REFERENCE ONLY · NO CREDENTIALS', { exact: true })
	).toBeVisible();
	await expect(
		desktopWizard.getByRole('link', { name: 'Open source for Reolink RLC-811A' })
	).toHaveAttribute('href', 'https://reolink.com/product/rlc-811a/');
	await expect(
		desktopWizard.getByRole('link', { name: 'Open CCTV Database catalog' })
	).toHaveAttribute('href', 'https://www.cctv-database.com/');
	await desktopWizard.getByLabel('Username').fill('operator');
	await desktopWizard.getByLabel('Password').fill('write-only-password');
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();

	await expect(page.getByRole('button', { name: 'Catalog streams applied' })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await expect(desktopWizard.getByLabel(/Recording stream/)).toHaveValue(
		'rtsp://192.0.2.77:554/Preview_01_main'
	);
	await expect(desktopWizard.getByLabel(/Live stream/)).toHaveValue(
		'rtsp://192.0.2.77:554/Preview_01_sub'
	);
	await expect(
		desktopWizard.getByText('KeepPeek received authenticated video evidence.')
	).toBeVisible();
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.77', onvifPort: 8000 }]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('reviews database, ONVIF, and measured stream evidence as separate sources', async ({
	page
}) => {
	await mockControlPeer(page, {
		discoveredCameras: [{ ...discoveredCamera, catalog: catalogCamera }],
		streamProbeResult: {
			manufacturer: 'Reolink',
			model: 'RLC-811A',
			firmware_version: 'v3.1.0',
			serial_number: 'SERIAL-REDACTED',
			hardware_id: 'IPC_523128M8MP',
			profiles: [
				{
					name: 'mainStream',
					stream: 'main',
					encoding: 'h265',
					resolution: '3840x2160',
					framerate: 25,
					bitrate_kbps: 8192,
					gop: 25,
					h264_profile: null,
					audio: null
				}
			]
		}
	});

	await page.goto('/cameras/new');
	await page.getByRole('button', { name: 'Discover cameras' }).click();
	await completeThroughReview(page);
	const review = page.locator('[data-desktop-camera-wizard]');
	await expect(review.getByText('DATABASE REFERENCE', { exact: true })).toBeVisible();
	await expect(review.getByText('ONVIF report', { exact: true })).toBeVisible();
	await expect(review.getByText('CAMERA REPORTED', { exact: true })).toBeVisible();
	await expect(review.getByText('Live media proof', { exact: true })).toBeVisible();
	await expect(review.getByText('MEASURED BY KEEPPEEK', { exact: true })).toBeVisible();
	await expect(review).toContainText('Firmware v3.1.0');
	await expect(review).toContainText('MAIN · H265 · 3840x2160 · 25 fps · 8192 kbps · GOP 25');
	await expect(review).toContainText('Field of view 105-31 horizontal');
	await expect(review).toContainText('Night vision hybrid');
	await expect(review).toContainText('Two-way audio');
});

test('tries a candidate camera automatically from the first screen', async ({ page }) => {
	const controls = await mockControlPeer(page, { discoveredCameras: [discoveredCamera] });

	await page.goto('/cameras/new');
	await page.getByRole('button', { name: 'Discover cameras' }).click();
	await page.getByRole('button', { name: /Front Gate/ }).click();
	const desktopWizard = page.locator('[data-desktop-camera-wizard]');
	await desktopWizard.getByLabel('Username').fill('operator');
	await desktopWizard.getByLabel('Password').fill('write-only-password');

	await expect(desktopWizard.locator('[data-onvif-probe-status]')).toBeVisible();
	await expect(desktopWizard.locator('[data-onvif-probe-status]')).toContainText(
		'ONVIF stream endpoints are ready on port 8000.'
	);
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.77', onvifPort: 8000 }]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('tries ONVIF from the first screen after direct address, model, and credentials', async ({
	page
}) => {
	const controls = await mockControlPeer(page, { cameraCatalogSearchResults: [catalogCamera] });

	await page.goto('/cameras/new');
	const manualCamera = page.locator('#manual-camera');
	await manualCamera.getByLabel('Address or RTSP URL').fill('192.0.2.88');
	await manualCamera.getByLabel('Camera model').fill('RLC-811A');
	await manualCamera.getByRole('button', { name: 'Search' }).click();
	await manualCamera.getByRole('button', { name: /Reolink RLC-811A/ }).click();
	await manualCamera.getByLabel('Username').fill('operator');
	await manualCamera.getByLabel('Password').fill('write-only-password');

	await expect(manualCamera.locator('[data-onvif-probe-status]')).toContainText(
		'ONVIF stream endpoints are ready on port 8000.'
	);
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByRole('heading', { name: 'Connection options' })).toBeVisible();
	await expect(page.locator('[data-desktop-camera-wizard]').getByLabel('ONVIF port')).toHaveValue(
		'8000'
	);
	expect(controls.catalogSearches).toEqual([
		{ query: 'RLC-811A', limit: undefined, ip: '192.0.2.88' }
	]);
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.88', onvifPort: 8000 }]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('keeps manual stream entry available while ONVIF lookup is pending', async ({ page }) => {
	let releaseProbe!: () => void;
	const streamProbeGate = new Promise<void>((resolve) => {
		releaseProbe = resolve;
	});
	const controls = await mockControlPeer(page, {
		discoveredCameras: [discoveredCamera],
		streamProbeGate
	});

	await page.goto('/cameras/new');
	await page.getByRole('button', { name: 'Discover cameras' }).click();
	await page.getByRole('button', { name: /Front Gate/ }).click();
	const desktopWizard = page.locator('[data-desktop-camera-wizard]');
	await desktopWizard.getByLabel('Username').fill('operator');
	await desktopWizard.getByLabel('Password').fill('write-only-password');
	await expect(desktopWizard.locator('[data-onvif-probe-status]')).toContainText(
		'Trying ONVIF at 192.0.2.77:8000…'
	);

	await page.getByRole('button', { name: 'Continue to streams' }).click();
	await page.getByRole('button', { name: 'Continue to streams' }).click();
	await expect(
		desktopWizard.getByText('Authenticating and waiting for video keyframes.', { exact: true })
	).toBeVisible();
	await expect(desktopWizard.getByLabel(/Recording stream/)).toBeEditable();
	await expect(desktopWizard.getByLabel(/Live stream/)).toBeEditable();

	releaseProbe();
	await expect(
		desktopWizard.getByText('KeepPeek received authenticated video evidence.', { exact: true })
	).toBeVisible();
	await expect(desktopWizard.getByLabel(/Recording stream/)).toHaveValue(
		'rtsp://192.0.2.77:554/onvif-main'
	);
	await expect(desktopWizard.getByLabel(/Live stream/)).toHaveValue(
		'rtsp://192.0.2.77:554/onvif-sub'
	);
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.77', onvifPort: 8000 }]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('auto-fills ONVIF stream endpoints before manual stream entry', async ({ page }) => {
	const controls = await mockControlPeer(page, { discoveredCameras: [discoveredCamera] });

	await page.goto('/cameras/new');
	await page.getByRole('button', { name: 'Discover cameras' }).click();
	await page.getByRole('button', { name: /Front Gate/ }).click();
	const desktopWizard = page.locator('[data-desktop-camera-wizard]');
	await desktopWizard.getByLabel('Username').fill('operator');
	await desktopWizard.getByLabel('Password').fill('write-only-password');
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();

	await expect(
		desktopWizard.getByText('KeepPeek received authenticated video evidence.')
	).toBeVisible();
	await expect(desktopWizard.getByLabel(/Recording stream/)).toHaveValue(
		'rtsp://192.0.2.77:554/onvif-main'
	);
	await expect(desktopWizard.getByLabel(/Live stream/)).toHaveValue(
		'rtsp://192.0.2.77:554/onvif-sub'
	);
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.77', onvifPort: 8000 }]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('searches the catalog for a manual camera address without configuring it', async ({
	page
}) => {
	const controls = await mockControlPeer(page, { cameraCatalogSearchResults: [catalogCamera] });

	await page.goto('/cameras/new');
	const manualCamera = page.locator('#manual-camera');
	await manualCamera.getByLabel('Address or RTSP URL').fill('192.0.2.88');
	await manualCamera.getByLabel('Camera model').fill('RLC-811A');
	await manualCamera.getByRole('button', { name: 'Search' }).click();
	await manualCamera.getByRole('button', { name: /Reolink RLC-811A/ }).click();

	await expect(
		manualCamera.getByRole('paragraph').filter({ hasText: 'Reolink RLC-811A' })
	).toBeVisible();
	await expect(manualCamera.getByRole('button', { name: /Reolink RLC-811A/ })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	expect(controls.catalogSearches).toEqual([
		{ query: 'RLC-811A', limit: undefined, ip: '192.0.2.88' }
	]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('selects a catalog model for a manual mobile camera address without configuring it', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, { cameraCatalogSearchResults: [catalogCamera] });

	await page.goto('/cameras/new');
	const mobileWizard = page.locator('[data-mobile-camera-wizard="find-connect"]');
	await mobileWizard.getByLabel('Address or RTSP URL').fill('192.0.2.88');
	await mobileWizard.getByRole('button', { name: 'Browse camera models' }).click();
	const catalogPicker = page.getByRole('dialog', { name: 'Camera model' });
	await catalogPicker.getByLabel('Camera model').fill('RLC-811A');
	await catalogPicker.getByRole('button', { name: 'Search' }).click();
	await catalogPicker.getByRole('button', { name: /Reolink RLC-811A/ }).click();

	await expect(catalogPicker).toHaveCount(0);
	await expect(mobileWizard.getByText('Reolink RLC-811A', { exact: true })).toBeVisible();
	await expect(
		mobileWizard.getByRole('link', { name: 'Open source for Reolink RLC-811A' })
	).toHaveAttribute('href', 'https://reolink.com/product/rlc-811a/');
	await expect(
		mobileWizard.getByRole('link', { name: 'Open CCTV Database catalog' })
	).toHaveAttribute('href', 'https://www.cctv-database.com/');
	expect(controls.catalogSearches).toEqual([
		{ query: 'RLC-811A', limit: undefined, ip: '192.0.2.88' }
	]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('tries ONVIF on mobile before opening streams', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, { cameraCatalogSearchResults: [catalogCamera] });

	await page.goto('/cameras/new');
	const mobileWizard = page.locator('[data-mobile-camera-wizard="find-connect"]');
	await mobileWizard.getByLabel('Address or RTSP URL').fill('192.0.2.88');
	await mobileWizard.getByRole('button', { name: 'Browse camera models' }).click();
	const catalogPicker = page.getByRole('dialog', { name: 'Camera model' });
	await catalogPicker.getByLabel('Camera model').fill('RLC-811A');
	await catalogPicker.getByRole('button', { name: 'Search' }).click();
	await catalogPicker.getByRole('button', { name: /Reolink RLC-811A/ }).click();
	await mobileWizard.getByLabel('Username').fill('operator');
	await mobileWizard.getByLabel('Password').fill('write-only-password');

	await expect(
		mobileWizard.getByText('ONVIF stream endpoints are ready on port 8000', { exact: true })
	).toBeVisible();
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.88', onvifPort: 8000 }]);
	await expect(page.locator('[data-mobile-camera-wizard="streams"]')).toHaveCount(0);
	expect(controls.cameraUpdates).toEqual([]);
});

test('keeps manual setup viable when no catalog model matches', async ({ page }) => {
	const controls = await mockControlPeer(page, { cameraCatalogSearchResults: [] });

	await page.goto('/cameras/new');
	const manualCamera = page.locator('#manual-camera');
	await manualCamera.getByLabel('Address or RTSP URL').fill('192.0.2.88');
	await expect(manualCamera.locator('#desktop-manual-address-status')).toContainText(
		'Address format is ready to use.'
	);
	await manualCamera.getByLabel('Camera model').fill('Unknown Model');
	await manualCamera.getByRole('button', { name: 'Search' }).click();

	await expect(manualCamera.getByText('No catalog results for Unknown Model')).toBeVisible();
	await expect(
		manualCamera.getByRole('link', { name: 'Research on CCTV Database' })
	).toHaveAttribute('href', 'https://www.cctv-database.com/');
	expect(controls.catalogSearches).toEqual([
		{ query: 'Unknown Model', limit: undefined, ip: '192.0.2.88' }
	]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('shows a mobile no-result state without hiding the manual address workflow', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, { cameraCatalogSearchResults: [] });

	await page.goto('/cameras/new');
	const mobileWizard = page.locator('[data-mobile-camera-wizard="find-connect"]');
	await mobileWizard.getByLabel('Address or RTSP URL').fill('192.0.2.88');
	await expect(mobileWizard.getByText('Address format is ready to use.')).toHaveCount(1);
	await mobileWizard.getByRole('button', { name: 'Browse camera models' }).click();
	const catalogPicker = page.getByRole('dialog', { name: 'Camera model' });
	await catalogPicker.getByLabel('Camera model').fill('Unknown Model');
	await catalogPicker.getByRole('button', { name: 'Search' }).click();
	await expect(catalogPicker.getByRole('status')).toContainText(
		'No catalog results for Unknown Model'
	);
	await expect(
		catalogPicker.getByRole('link', { name: 'Research on CCTV Database' })
	).toHaveAttribute('href', 'https://www.cctv-database.com/');
	await expect(mobileWizard.getByLabel('Address or RTSP URL')).toHaveValue('192.0.2.88');
	expect(controls.cameraUpdates).toEqual([]);
});

test('applies mobile catalog stream suggestions with visible confirmation', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, {
		cameraCatalogSearchResults: [catalogCamera],
		streamProbeResult: { main_rtsp_url: null, sub_rtsp_url: null }
	});

	await page.goto('/cameras/new');
	const findStage = page.locator('[data-mobile-camera-wizard="find-connect"]');
	await findStage.getByLabel('Address or RTSP URL').fill('192.0.2.88');
	await findStage.getByRole('button', { name: 'Browse camera models' }).click();
	const catalogPicker = page.getByRole('dialog', { name: 'Camera model' });
	await catalogPicker.getByLabel('Camera model').fill('RLC-811A');
	await catalogPicker.getByRole('button', { name: 'Search' }).click();
	await catalogPicker.getByRole('button', { name: /Reolink RLC-811A/ }).click();
	await findStage.getByLabel('Username').fill('operator');
	await findStage.getByLabel('Password').fill('write-only-password');
	await findStage.getByRole('button', { name: 'Connect' }).click();

	const streams = page.locator('[data-mobile-camera-wizard="streams"]');
	await expect(streams.locator('button[aria-pressed="true"]')).toHaveCount(1);
	await expect(streams.getByLabel('Recording stream')).toHaveValue(
		'rtsp://192.0.2.88:554/Preview_01_main'
	);
	await expect(streams.getByLabel('Live stream')).toHaveValue(
		'rtsp://192.0.2.88:554/Preview_01_sub'
	);
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.88', onvifPort: 8000 }]);
	expect(controls.cameraUpdates).toEqual([]);
});

test('validates and cancels a manual RTSP draft at the authored mobile viewport', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	let writes = 0;
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') writes += 1;
	});

	await page.goto('/cameras/new');
	const mobileWizard = page.locator('[data-mobile-camera-wizard="find-connect"]');
	await mobileWizard.getByRole('button', { name: 'Connect' }).click();
	await expect(page.getByRole('alert')).toContainText(
		'Choose a discovered camera or enter an address.'
	);
	await mobileWizard.getByLabel('Address or RTSP URL').fill('rtsp://192.0.2.99:8554/live/main');
	await mobileWizard.getByLabel('Username').fill('operator');
	await mobileWizard.getByLabel('Password').fill('write-only-password');
	await mobileWizard.getByRole('button', { name: 'Connect' }).click();
	await expect(page.locator('[data-mobile-camera-wizard="streams"]')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Review' })).toBeInViewport();

	await page.keyboard.press('Escape');
	await expect(page).toHaveURL(/\/cameras$/);
	expect(writes).toBe(0);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('renders Board 25 mobile find and connect without writing configuration', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, { discoveredCameras: [discoveredCamera] });

	await page.goto('/cameras/new');
	const stage = page.locator('[data-mobile-camera-wizard="find-connect"]');
	await stage.getByRole('button', { name: 'Scan this network' }).click();
	await expect(stage.getByRole('button', { name: /Front Gate/ })).toBeVisible();
	await stage.getByRole('button', { name: /Front Gate/ }).click();
	await expect(stage).toContainText('WRITE-ONLY DRAFT');
	await stage.getByLabel('Username').fill('operator');
	await stage.getByLabel('Password').fill('write-only-password');
	await expect(stage.getByRole('button', { name: 'Connect' })).toBeInViewport();
	expect(controls.cameraUpdates).toEqual([]);
});

test('renders Board 25 with authenticated video and keyframe proof', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, { discoveredCameras: [discoveredCamera] });
	await reachMobileStreams(page);

	const stage = page.locator('[data-mobile-camera-wizard="streams"]');
	await expect(stage).toContainText('H264 · 1920x1080 · KEYFRAME');
	await expect(stage.getByLabel('Recording stream')).toHaveValue(
		'rtsp://192.0.2.77:554/onvif-main'
	);
	await expect(stage).toContainText('Required video streams and keyframes are verified.');
	await expect(stage.getByRole('button', { name: 'Review' })).toBeInViewport();
	expect(controls.cameraUpdates).toEqual([]);
});

test('renders Board 25 mobile review and writes only from the final action', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, {
		discoveredCameras: [discoveredCamera],
		cameraUpdateResult: { camera: savedCamera, restart_required: true }
	});
	await reachMobileStreams(page);

	const streams = page.locator('[data-mobile-camera-wizard="streams"]');
	await streams.getByLabel('Recording stream').fill('rtsp://192.0.2.77:8554/live/main');
	await streams.getByLabel('Live stream').fill('rtsp://192.0.2.77:8554/live/sub');
	await streams.getByRole('button', { name: 'Verify streams' }).click();
	await expect(streams).toContainText('Required video streams and keyframes are verified.');
	await streams.getByRole('button', { name: 'Review' }).click();

	const review = page.locator('[data-mobile-camera-wizard="review"]');
	await expect(review.getByLabel('CAMERA NAME')).toHaveValue('Front Gate');
	await expect(review).toContainText('Retention impact unavailable');
	await expect(review).toContainText('Saving is the first configuration write.');
	await expect(review).toContainText('Authenticated video and required keyframes are verified.');
	expect(controls.cameraUpdates).toEqual([]);
	await review.getByRole('button', { name: 'Save camera' }).click();
	await expect(page.getByRole('region', { name: 'Camera saved' })).toBeVisible();
	expect(controls.cameraUpdates).toEqual([
		{
			ip: '192.0.2.77',
			update: {
				display_name: 'Front Gate',
				username: 'operator',
				password: 'write-only-password',
				onvif_port: 8000,
				http_port: 80,
				main_rtsp_url: 'rtsp://192.0.2.77:8554/live/main',
				sub_rtsp_url: 'rtsp://192.0.2.77:8554/live/sub',
				uid: null,
				backend: 'reo-proto',
				transport: 'tcp',
				record_generic_motion_events: false,
				recording_mode: 'event-boost',
				event_recording_duration_secs: 60
			}
		}
	]);
});
