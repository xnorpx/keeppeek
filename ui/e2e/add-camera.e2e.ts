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
	health: null,
	model: null
};

async function completeThroughReview(page: Page): Promise<void> {
	const desktopWizard = page.locator('[data-desktop-camera-wizard]');
	await page.getByRole('button', { name: /Front Gate/ }).click();
	await page.getByRole('button', { name: 'Continue' }).click();
	await desktopWizard.getByLabel('Username').fill('operator');
	await desktopWizard.getByLabel('Password').fill('write-only-password');
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByText('Decoded stream evidence is unavailable before save.')).toBeVisible();
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

test('renders Board 25 mobile stream declarations without claiming decoded proof', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, { discoveredCameras: [discoveredCamera] });
	await reachMobileStreams(page);

	const stage = page.locator('[data-mobile-camera-wizard="streams"]');
	await expect(stage).toContainText('PROBE UNAVAILABLE');
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
