import { expect, test, type Page } from '@playwright/test';
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
		page.getByText('ONVIF reported candidate RTSP endpoints.', { exact: true })
	).toBeVisible();
	await desktopWizard.getByLabel(/Recording stream/).fill('rtsp://192.0.2.77:8554/live/main');
	await desktopWizard.getByLabel(/Live stream/).fill('rtsp://192.0.2.77:8554/live/sub');
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
	await expect(
		page.getByRole('progressbar', { name: 'Five-second discovery window' })
	).toHaveAttribute('aria-valuemax', '5000');
	releaseDiscovery();
	await expect(page.getByRole('button', { name: /Front Gate/ })).toBeVisible();
	await expect(progress).toHaveCount(0);
	expect(controls.discoverySubnets).toEqual([[1]]);

	await completeThroughReview(page);
	expect(controls.cameraUpdates).toEqual([]);
	await expect(page.getByText('write-only-password', { exact: true })).toHaveCount(0);
	await page.getByRole('button', { name: 'Save camera' }).click();

	await expect(page.getByRole('region', { name: 'Camera saved' })).toBeVisible();
	await expect(page.getByText('The server reported that a restart is required')).toBeVisible();
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
				transport: 'tcp'
			}
		}
	]);
	await expect(page.getByText('write-only-password', { exact: true })).toHaveCount(0);
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
	await expect(
		page
			.locator('[data-desktop-camera-wizard]')
			.getByText('rtsp://192.0.2.77:8554/live/main', { exact: true })
	).toBeVisible();
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
		desktopWizard.getByText('Catalog candidate RTSP endpoints are applied.')
	).toBeVisible();
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.77', onvifPort: 8000 }]);
	expect(controls.cameraUpdates).toEqual([]);
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
		'ONVIF stream endpoints are ready on port 80.'
	);
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByRole('heading', { name: 'Connection options' })).toBeVisible();
	await expect(page.locator('[data-desktop-camera-wizard]').getByLabel('ONVIF port')).toHaveValue(
		'80'
	);
	expect(controls.catalogSearches).toEqual([
		{ query: 'RLC-811A', limit: undefined, ip: '192.0.2.88' }
	]);
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.88', onvifPort: null }]);
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
		desktopWizard.getByText('ONVIF lookup is in progress.', { exact: true })
	).toBeVisible();
	await expect(desktopWizard.getByLabel(/Recording stream/)).toBeEditable();
	await expect(desktopWizard.getByLabel(/Live stream/)).toBeEditable();

	releaseProbe();
	await expect(
		desktopWizard.getByText('ONVIF reported candidate RTSP endpoints.', { exact: true })
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

	await expect(desktopWizard.getByText('ONVIF reported candidate RTSP endpoints.')).toBeVisible();
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
		mobileWizard.getByText('ONVIF stream endpoints are ready on port 80', { exact: true })
	).toBeVisible();
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.88', onvifPort: null }]);
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
	await expect(streams.locator('button[aria-pressed="true"]')).toHaveCount(2);
	await expect(streams.getByLabel('Recording stream')).toHaveValue(
		'rtsp://192.0.2.88:554/Preview_01_main'
	);
	await expect(streams.getByLabel('Live stream')).toHaveValue(
		'rtsp://192.0.2.88:554/Preview_01_sub'
	);
	expect(controls.streamProbes).toEqual([{ ip: '192.0.2.88', onvifPort: null }]);
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

test('renders Board 25 ONVIF candidate streams without claiming decoded proof', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, { discoveredCameras: [discoveredCamera] });
	await reachMobileStreams(page);

	const stage = page.locator('[data-mobile-camera-wizard="streams"]');
	await expect(stage).toContainText('ONVIF REPORTED');
	await expect(stage.getByLabel('Recording stream')).toHaveValue(
		'rtsp://192.0.2.77:554/onvif-main'
	);
	await expect(stage).toContainText('Codec evidence is unavailable');
	await expect(stage).not.toContainText('DECODING');
	await expect(stage).not.toContainText('TESTED');
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
	await streams.getByRole('button', { name: 'Review' }).click();

	const review = page.locator('[data-mobile-camera-wizard="review"]');
	await expect(review.getByLabel('CAMERA NAME')).toHaveValue('Front Gate');
	await expect(review).toContainText('Retention impact unavailable');
	await expect(review).toContainText('Saving is the first configuration write.');
	await expect(review).not.toContainText('Connection and both streams passed.');
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
				transport: 'tcp'
			}
		}
	]);
});
