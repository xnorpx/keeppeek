import { expect, test } from '@playwright/test';
import type { Locator, Page } from '@playwright/test';
import { mockControlPeer, type HealthFixture } from './fixtures/control-peer';
import { presentMockVideoFrame } from './fixtures/media';
import { mixedCameras, mixedHealth, mockMixedHealth } from './fixtures/peek';

async function expectFrontDoorCameraInformation(page: Page, scope: Locator) {
	const trigger = scope.getByRole('button', { name: 'Front Door camera information' });
	await expect(trigger).toHaveAttribute('data-peek-camera-label', 'Front Door');
	await expect(trigger).toContainText('Front Door');
	await expect(scope.getByText('Front Door', { exact: true })).toHaveCount(1);
	await expect(trigger.locator('span').first()).toHaveClass(/bg-healthy/);
	const [scopeBounds, triggerBounds] = await Promise.all([
		scope.boundingBox(),
		trigger.boundingBox()
	]);
	expect(scopeBounds).not.toBeNull();
	expect(triggerBounds).not.toBeNull();
	if (!scopeBounds || !triggerBounds) throw new Error('Camera information geometry is unavailable');
	expect(triggerBounds.width).toBeGreaterThan(triggerBounds.height);
	expect(
		Math.abs(scopeBounds.x + scopeBounds.width - triggerBounds.x - triggerBounds.width)
	).toBeLessThanOrEqual(12);
	await trigger.click();
	const dialog = page.getByRole('dialog', { name: 'Front Door camera information' });
	await expect(dialog).toBeVisible();
	expect(
		await dialog.evaluate((element) => {
			const bounds = element.getBoundingClientRect();
			const hit = document.elementFromPoint(
				bounds.left + bounds.width / 2,
				bounds.top + bounds.height / 2
			);
			return hit !== null && element.contains(hit);
		})
	).toBe(true);
	await expect(page.locator('[data-web-rtc-recording="front-door"]')).toHaveText(
		'Sub stream · recording'
	);
	await expect(page.locator('[data-web-rtc-recording="front-door"]')).toHaveAttribute(
		'data-recording-state',
		'recording'
	);
	await expect(page.locator('[data-camera-session-duration="front-door"]')).toHaveText('10m 00s');
	await expect(page.locator('[data-main-recorded-duration="front-door"]')).toHaveText('8m 00s');
	await expect(page.locator('[data-sub-recorded-duration="front-door"]')).toHaveText('5m 00s');
	await expect(page.locator('[data-total-recorded-duration="front-door"]')).toHaveText('13m 00s');
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
}

test('renders the KeepPeek dashboard without configured cameras', async ({ page }) => {
	await mockControlPeer(page, { health: { status: 'healthy', cameras: [] } });

	await page.goto('/');

	await expect(page).toHaveTitle('Dashboard - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toHaveCount(1);
	await expect(page.getByRole('heading', { name: 'Peek', exact: true })).toHaveCount(0);
	await expect(page.locator('[data-shell-status-indicator="cameras"]')).toHaveText('0/0');
	await expect(page.getByText('No cameras configured.')).toBeVisible();
});

test('Board 6 renders healthy, degraded, stale, and offline Paper tile states', async ({
	page
}) => {
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	await mockMixedHealth(page);
	await page.goto('/');

	const fleetStatus = page.locator('[data-shell-status-indicators]');
	await expect(fleetStatus.locator('[data-shell-status-indicator="server"]')).toHaveText('1/1');
	await expect(fleetStatus.locator('[data-shell-status-indicator="cameras"]')).toHaveText('3/4');
	await expect(fleetStatus.locator('[data-shell-status-indicator="recording"]')).toHaveText('1/4');
	await expect(fleetStatus.locator('[data-shell-status-indicator="clients"]')).toHaveText('0/0');
	await expect(
		fleetStatus.locator('[data-shell-status-indicator="server"] span').first()
	).toHaveClass(/bg-activity/);
	await fleetStatus.locator('[data-shell-status-indicator="cameras"]').hover();
	await expect(page.getByText('Cameras connected: 3 of 4', { exact: true })).toBeVisible();
	await expect(page.locator('[data-peek-runtime-telemetry]')).toHaveCount(0);

	await expect(page.locator('[data-peek-camera="front-door"]')).toHaveAttribute(
		'data-peek-camera-state',
		'healthy'
	);
	await expect(page.locator('[data-peek-camera="porch"]')).toHaveAttribute(
		'data-peek-camera-state',
		'degraded'
	);
	await expect(page.locator('[data-peek-camera="alley"]')).toHaveAttribute(
		'data-peek-camera-state',
		'stale'
	);
	await expect(page.locator('[data-peek-camera="back-yard"]')).toHaveAttribute(
		'data-peek-camera-state',
		'offline'
	);
	await expect(
		page.locator('[data-peek-camera="porch"] [data-peek-camera-region="evidence"]')
	).toContainText('DEGRADED — 14% frames dropped');
	await expect(page.getByText('Stream health report is stale')).toBeVisible();
	await expect(page.getByText('Authentication failed')).toBeVisible();
	await expect(page.getByRole('link', { name: 'Diagnose' })).toBeVisible();
	await expect(page.locator('[data-peek-camera="front-door"]')).not.toContainText('REC');
	await expect(page.locator('[data-peek-camera="back-yard"]')).not.toContainText('REC');
	await expect(page.getByText(/last frame/i)).toHaveCount(0);
	await expect(page.getByText(/SUB ·/i)).toHaveCount(0);
	const frontDoor = page.locator('[data-peek-camera="front-door"]');
	const frontDoorLabel = frontDoor.locator('[data-peek-camera-label]');
	const frontDoorDiagnostics = frontDoor.getByRole('button', {
		name: 'Front Door camera information'
	});
	await expect(page.locator('[data-peek-camera-status]')).toHaveCount(0);
	await expect(frontDoorLabel).toBeVisible();
	await expect(frontDoorDiagnostics).toBeVisible();
	await frontDoorDiagnostics.click();
	await expect(page.locator('[data-web-rtc-recording="front-door"]')).toHaveText(
		'Sub stream · recording'
	);
	await expect(page.locator('[data-web-rtc-recording="front-door"]')).toHaveAttribute(
		'data-recording-state',
		'recording'
	);
	await expect(page.locator('[data-camera-session-duration="front-door"]')).toHaveText('10m 00s');
	await expect(page.locator('[data-main-recorded-duration="front-door"]')).toHaveText('8m 00s');
	await expect(page.locator('[data-sub-recorded-duration="front-door"]')).toHaveText('5m 00s');
	await expect(page.locator('[data-total-recorded-duration="front-door"]')).toHaveText('13m 00s');
	await page.keyboard.press('Escape');
	const porchDiagnostics = page
		.locator('[data-peek-camera="porch"]')
		.getByRole('button', { name: 'Porch camera information' });
	await porchDiagnostics.click();
	await expect(page.locator('[data-web-rtc-recording="porch"]')).toHaveText(
		'Sub stream · not progressing'
	);
	await expect(page.locator('[data-web-rtc-recording="porch"]')).toHaveAttribute(
		'data-recording-state',
		'not-progressing'
	);
	await page.keyboard.press('Escape');
	const [frontDoorBounds, labelBounds, diagnosticsBounds] = await Promise.all([
		frontDoor.boundingBox(),
		frontDoorLabel.boundingBox(),
		frontDoorDiagnostics.boundingBox()
	]);
	expect(frontDoorBounds).not.toBeNull();
	expect(labelBounds).not.toBeNull();
	expect(diagnosticsBounds).not.toBeNull();
	if (!frontDoorBounds || !labelBounds || !diagnosticsBounds) {
		throw new Error('Peek camera header geometry is unavailable');
	}
	expect(labelBounds.x).toBeGreaterThan(frontDoorBounds.x + frontDoorBounds.width / 2);
	expect(labelBounds).toEqual(diagnosticsBounds);
	await page.locator('[data-peek-camera="porch"]').hover();
	await expect(page.getByRole('button', { name: 'Rewind Porch' })).toHaveCount(0);
	await page.locator('[data-peek-camera="back-yard"]').hover();
	await expect(page.getByRole('button', { name: 'Rewind Back Yard' })).toHaveCount(0);
	expect(browserErrors).toEqual([]);
});

test('opens camera information from a dashboard tile', async ({ page }) => {
	await mockMixedHealth(page);
	await page.goto('/');

	await expectFrontDoorCameraInformation(page, page.locator('[data-peek-camera="front-door"]'));
});

test('opens camera information from a focused dashboard tile', async ({ page }) => {
	await mockMixedHealth(page);
	await page.goto('/');

	await page.getByRole('button', { name: 'Focus Front Door live view' }).click();
	const focus = page.getByRole('region', { name: 'Front Door focus' });
	await expectFrontDoorCameraInformation(page, focus.locator('[data-peek-focus-history]'));
});

test('opens a full-shell focus view with consolidated camera controls and complete filmstrip', async ({
	page
}) => {
	await page.setViewportSize({ width: 1188, height: 624 });
	await mockMixedHealth(page);
	await page.goto('/');

	await page.getByRole('button', { name: 'Focus Front Door live view' }).click();
	const focus = page.getByRole('region', { name: 'Front Door focus' });
	const cameraControls = focus.locator('[data-live-video-camera-controls]');
	const filmstrip = focus.getByLabel('Camera filmstrip');
	await expect(focus.locator('[data-peek-focus-floatie]')).toHaveCount(0);
	await expect(cameraControls.getByText('Front Door', { exact: true })).toBeVisible();
	await expect(
		cameraControls.getByRole('link', { name: 'Open Front Door camera' })
	).toHaveAttribute('href', '/camera?camera=front-door');
	await expect(
		cameraControls.getByRole('button', { name: 'Front Door camera information' })
	).toBeVisible();
	await expect(filmstrip.locator('[data-focus-camera-option]')).toHaveCount(mixedCameras.length);
	await expect(filmstrip.getByRole('button', { name: 'Focus Front Door' })).toHaveAttribute(
		'aria-pressed',
		'true'
	);

	const [mainBounds, stageBounds] = await Promise.all([
		page.locator('[data-shell-main]').boundingBox(),
		focus.locator('[data-peek-focus-stage]').boundingBox()
	]);
	expect(stageBounds).toEqual(mainBounds);

	await page.setViewportSize({ width: 390, height: 844 });
	const [mobileMain, mobileStage, mobileCameraControls, mobileFocusControls, mobileFilmstrip] =
		await Promise.all([
			page.locator('[data-shell-main]').boundingBox(),
			focus.locator('[data-peek-focus-stage]').boundingBox(),
			cameraControls.boundingBox(),
			focus.locator('.focus-controls').boundingBox(),
			filmstrip.boundingBox()
		]);
	expect(mobileStage).toEqual(mobileMain);
	if (!mobileMain || !mobileCameraControls || !mobileFocusControls || !mobileFilmstrip) {
		throw new Error('Mobile Viewer control geometry is unavailable');
	}
	expect(mobileCameraControls.x + mobileCameraControls.width).toBeLessThanOrEqual(
		mobileMain.x + mobileMain.width
	);
	expect(mobileFocusControls.y).toBeGreaterThanOrEqual(
		mobileCameraControls.y + mobileCameraControls.height
	);
	expect(mobileFilmstrip.y).toBeGreaterThan(mobileFocusControls.y + mobileFocusControls.height);
	expect(mobileFilmstrip.y + mobileFilmstrip.height).toBeLessThanOrEqual(
		mobileMain.y + mobileMain.height
	);
});

test('separates Dashboard and Viewer while remembering the last camera', async ({ page }) => {
	await mockMixedHealth(page);

	await page.goto('/viewer');
	await expect(page).toHaveURL(/\/viewer\?camera=front-door$/);
	await expect(page.getByRole('region', { name: 'Front Door focus' })).toBeVisible();
	await page.getByLabel('Camera filmstrip').getByRole('button', { name: 'Focus Porch' }).click();
	await expect(page).toHaveURL(/\/viewer\?camera=porch$/);

	await page.getByRole('link', { name: 'Dashboard' }).click();
	await expect(page).toHaveURL(/\/$/);
	await expect(page.locator('[data-peek-wall]')).toBeVisible();
	await page.getByRole('link', { name: 'Viewer' }).click();
	await expect(page).toHaveURL(/\/viewer\?camera=porch$/);
	await expect(page.getByRole('region', { name: 'Porch focus' })).toBeVisible();
});

test('renders the focus filmstrip as video-only camera switches', async ({ page }) => {
	await mockMixedHealth(page);
	await page.goto('/');

	await page.getByRole('button', { name: 'Focus Porch live view' }).click();
	const focus = page.getByRole('region', { name: 'Porch focus' });
	const filmstrip = focus.getByLabel('Camera filmstrip');
	await expect(filmstrip.locator('[data-focus-camera-option] video')).toHaveCount(
		mixedCameras.length
	);
	await expect(filmstrip.getByText('Front Door', { exact: true })).toHaveCount(0);
	await expect(filmstrip.getByRole('button', { name: /camera information$/ })).toHaveCount(0);
	for (const camera of mixedCameras) {
		const option = filmstrip.locator(`[data-focus-camera-option="${camera.id}"]`);
		await expect(option.locator('video')).toHaveCount(1);
		await expect(option.locator('[data-stream]')).toHaveAttribute('data-stream', 'sub');
		await expect(option.locator('button')).toHaveCount(1);
		await expect(option.getByRole('button')).toHaveAttribute('aria-label', `Focus ${camera.name}`);
	}
	await filmstrip.getByRole('button', { name: 'Focus Front Door' }).click();
	await expect(page).toHaveURL(/\/viewer\?camera=front-door$/);
});

test('refreshes startup health evidence in place without reopening Peek', async ({ page }) => {
	test.setTimeout(30_000);
	const staleEvidence = mixedHealth.cameras?.find((camera) => camera.id === 'alley');
	const offlineEvidence = mixedHealth.cameras?.find((camera) => camera.id === 'back-yard');
	if (!staleEvidence || !offlineEvidence)
		throw new Error('mixed health transitions are incomplete');
	const withFrontDoorEvidence = (evidence: typeof staleEvidence) => ({
		...mixedHealth,
		cameras: mixedHealth.cameras?.map((camera) =>
			camera.id === 'front-door' ? { ...evidence, id: 'front-door' } : camera
		)
	});
	const initialHealth: HealthFixture = {
		...mixedHealth,
		cameras: mixedHealth.cameras?.map((camera) =>
			camera.id === 'front-door'
				? {
						...camera,
						state: 'unknown',
						reason: 'evidence_unavailable',
						detail: 'Required camera evidence is unavailable'
					}
				: camera
		)
	};
	await mockControlPeer(page, {
		cameras: mixedCameras,
		healthSequence: [
			initialHealth,
			mixedHealth,
			withFrontDoorEvidence(staleEvidence),
			withFrontDoorEvidence(offlineEvidence),
			mixedHealth
		]
	});
	await page.goto('/');

	const tile = page.locator('[data-peek-camera="front-door"]');
	await expect(tile).toHaveAttribute('data-peek-camera-state', 'unknown');
	await expect(tile).toHaveAttribute('data-peek-camera-state', 'healthy', { timeout: 12_000 });
	await expect(tile.locator('[data-peek-camera-status]')).toHaveCount(0);
	await expect(tile).toHaveAttribute('data-peek-camera-state', 'stale', { timeout: 7_000 });
	await expect(tile).toContainText('STALE');
	await expect(tile).toHaveAttribute('data-peek-camera-state', 'offline', { timeout: 7_000 });
	await expect(tile).toHaveAttribute('data-peek-camera-state', 'healthy', { timeout: 7_000 });
});

test('keeps mixed Peek states usable at the authored mobile viewport', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockMixedHealth(page);
	await page.goto('/');

	await expect(page.locator('[data-peek-camera]')).toHaveCount(4);
	await expect(page.locator('[data-peek-camera="back-yard"]')).toBeVisible();
	await expect(page.getByRole('button', { name: /^Rewind / })).toHaveCount(0);
	await expect
		.poll(() =>
			page.locator('[data-peek-camera="front-door"]').evaluate((element) => {
				const bounds = element.getBoundingClientRect();
				return [Math.round(bounds.width), Math.round(bounds.height)];
			})
		)
		.toEqual([390, 219]);
	await expect
		.poll(() =>
			page.locator('[data-peek-camera="porch"]').evaluate((element) => {
				const bounds = element.getBoundingClientRect();
				return [Math.round(bounds.width), Math.round(bounds.height)];
			})
		)
		.toEqual([190, 120]);
	for (const tile of await page.locator('[data-peek-camera]').all()) {
		const overlaps = await tile.evaluate((element) => {
			const regions = Array.from(
				element.querySelectorAll<HTMLElement>('[data-peek-camera-region]')
			).filter((region) => {
				const bounds = region.getBoundingClientRect();
				return bounds.width > 0 && bounds.height > 0;
			});
			return regions.flatMap((region, regionIndex) => {
				const bounds = region.getBoundingClientRect();
				return regions.slice(regionIndex + 1).flatMap((otherRegion) => {
					const otherBounds = otherRegion.getBoundingClientRect();
					const overlapsHorizontally =
						bounds.left < otherBounds.right && bounds.right > otherBounds.left;
					const overlapsVertically =
						bounds.top < otherBounds.bottom && bounds.bottom > otherBounds.top;
					return overlapsHorizontally && overlapsVertically
						? [`${region.dataset.peekCameraRegion}:${otherRegion.dataset.peekCameraRegion}`]
						: [];
				});
			});
		});
		expect(overlaps).toEqual([]);
	}
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('returns from Viewer to the coordinated Dashboard wall', async ({ page }) => {
	const cameraHealth = mixedHealth.cameras?.[0];
	if (!cameraHealth) throw new Error('mixed health fixture must include Front Door');
	const camera = {
		...mixedCameras[0],
		profiles: [
			{
				name: 'Main',
				stream: 'main' as const,
				encoding: 'h264' as const,
				resolution: '1920x1080',
				framerate: 25
			},
			{
				name: 'Sub',
				stream: 'sub' as const,
				encoding: 'h264' as const,
				resolution: '640x360',
				framerate: 15
			}
		]
	};
	await mockControlPeer(page, {
		cameras: [camera],
		health: { ...mixedHealth, cameras: [cameraHealth] }
	});
	await page.goto('/');

	const wall = page.locator('[data-peek-wall]');
	await expect(wall).toHaveAttribute('data-peek-wall-state', 'staging');
	await presentMockVideoFrame(wall.locator('video'));
	await expect(wall).toHaveAttribute('data-peek-wall-reveal', 'frames');

	await page.getByRole('button', { name: 'Focus Front Door live view' }).click();
	const focus = page.getByRole('region', { name: 'Front Door focus' });
	const focusedVideo = focus.locator('[data-peek-focus-stage] [data-camera-id="front-door"]');
	await expect(focus).toBeVisible();
	await expect(page.locator('[data-peek-transition="viewer"]')).toHaveCount(0);
	await expect(focusedVideo).toHaveAttribute('data-stream', 'sub');
	await expect(focusedVideo).not.toHaveAttribute('data-pending-stream');
	await expect(focus.locator('[data-peek-focus-stage] [data-peek-cached-frame]')).toBeVisible();
	await presentMockVideoFrame(focusedVideo.locator('video'));
	await expect(focusedVideo).toHaveAttribute('data-pending-stream', 'main');
	const returnMs = await page
		.getByRole('link', { name: 'Dashboard', exact: true })
		.evaluate(async (link) => {
			if (!(link instanceof HTMLAnchorElement)) throw new Error('Expected Dashboard link');
			const startedAt = performance.now();
			link.click();
			while (document.querySelector('[data-peek-focus-stage]')) {
				await new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()));
			}
			return performance.now() - startedAt;
		});

	expect(returnMs).toBeLessThanOrEqual(100);
	await expect(page).toHaveURL(/\/$/);
	await expect(focus).toHaveCount(0);
	await expect(page.locator('[data-peek-transition="dashboard"]')).toHaveCount(0);
	await expect(wall).toHaveAttribute('aria-hidden', 'false');
	await expect(wall).toHaveAttribute('data-peek-wall-reveal', 'frames');
});

test('reduces background decoding to one frame per second after five minutes', async ({ page }) => {
	await page.clock.install();
	const cameras = [
		{ id: 'front-door', name: 'Front Door', ip: '192.0.2.10' },
		{ id: 'garage', name: 'Garage', ip: '192.0.2.11' }
	].map((camera) => ({
		...camera,
		manufacturer: null,
		model: null,
		firmware_version: null,
		is_reolink: false,
		profiles: [
			{
				name: 'Main',
				stream: 'main' as const,
				encoding: 'h264' as const,
				resolution: '1920x1080',
				framerate: 25
			},
			{
				name: 'Sub',
				stream: 'sub' as const,
				encoding: 'h264' as const,
				resolution: '640x360',
				framerate: 15
			}
		]
	}));
	const requests = await mockControlPeer(page, {
		cameras,
		health: {
			status: 'healthy',
			cameras: cameras.map((camera) => ({
				id: camera.id,
				state: 'healthy',
				lifecycle: 'Connected',
				last_error: null,
				streams: []
			}))
		}
	});
	await page.goto('/');
	for (const video of await page.locator('[data-peek-wall] video').all()) {
		await presentMockVideoFrame(video);
	}

	await page.getByRole('button', { name: 'Focus Front Door live view' }).click();
	const focus = page.getByRole('region', { name: 'Front Door focus' });
	await expect(focus).toHaveAttribute('data-background-stream-rate', 'full');
	const initialUnsubscriptions = requests.mediaUnsubscriptions.length;

	await page.clock.fastForward(299_999);
	expect(requests.mediaUnsubscriptions).toHaveLength(initialUnsubscriptions);
	await page.clock.fastForward(1);
	await expect(focus).toHaveAttribute('data-background-stream-rate', '1fps');
	const increaseFps = focus.getByRole('button', { name: 'Increase background FPS' });
	await expect(increaseFps).toBeVisible();

	await presentMockVideoFrame(page.locator('[data-peek-camera="garage"] video'));
	await expect
		.poll(() => requests.mediaUnsubscriptions.length)
		.toBeGreaterThan(initialUnsubscriptions);
	const subscriptionsBeforePulse = requests.mediaSubscriptions.length;
	await page.clock.fastForward(1_000);
	await expect
		.poll(() => requests.mediaSubscriptions.length)
		.toBeGreaterThan(subscriptionsBeforePulse);
	const unsubscriptionsBeforeDeadline = requests.mediaUnsubscriptions.length;
	await page.clock.fastForward(750);
	await expect
		.poll(() => requests.mediaUnsubscriptions.length)
		.toBeGreaterThan(unsubscriptionsBeforeDeadline);

	await increaseFps.click();
	await expect(focus).toHaveAttribute('data-background-stream-rate', 'full');
	await expect(increaseFps).toHaveCount(0);
	const resetUnsubscriptions = requests.mediaUnsubscriptions.length;
	await page.clock.fastForward(299_999);
	expect(requests.mediaUnsubscriptions).toHaveLength(resetUnsubscriptions);
	await page.clock.fastForward(1);
	await expect(focus).toHaveAttribute('data-background-stream-rate', '1fps');
});

test('keeps focused-live preferences device-local per camera and separate from the wall', async ({
	page
}) => {
	const cameras = [
		{ id: 'front-door', name: 'Front Door', ip: '192.0.2.10' },
		{ id: 'porch', name: 'Porch', ip: '192.0.2.11' }
	].map((camera) => ({
		...camera,
		manufacturer: 'ONVIF',
		model: null,
		firmware_version: null,
		is_reolink: false,
		profiles: [
			{
				name: 'Main',
				stream: 'main' as const,
				encoding: 'h264',
				resolution: '1920x1080',
				framerate: 25
			},
			{
				name: 'Sub',
				stream: 'sub' as const,
				encoding: 'h264',
				resolution: '640x360',
				framerate: 15
			}
		]
	}));
	await mockControlPeer(page, {
		cameras,
		health: {
			status: 'healthy',
			cameras: cameras.map((camera) => ({
				id: camera.id,
				state: 'healthy',
				lifecycle: 'Connected',
				last_error: null,
				streams: []
			}))
		}
	});
	await page.goto('/viewer?camera=front-door');
	let focus = page.getByRole('region', { name: 'Front Door focus' });
	await focus.getByRole('button', { name: 'Sub', exact: true }).click();
	await expect(focus).toHaveAttribute('data-focused-live-preference', 'sub');
	await expect(focus).toHaveAttribute('data-focused-live-selected-variant', 'sub');

	await page.goto('/viewer?camera=porch');
	focus = page.getByRole('region', { name: 'Porch focus' });
	await expect(focus).toHaveAttribute('data-focused-live-preference', 'auto');
	await focus.getByRole('button', { name: 'High', exact: true }).click();
	await expect(focus).toHaveAttribute('data-focused-live-preference', 'high');
	await expect(focus).toHaveAttribute('data-focused-live-selected-variant', 'main');

	await page.goto('/viewer?camera=front-door');
	focus = page.getByRole('region', { name: 'Front Door focus' });
	await expect(focus).toHaveAttribute('data-focused-live-preference', 'sub');

	await page.reload();
	focus = page.getByRole('region', { name: 'Front Door focus' });
	await expect(focus).toHaveAttribute('data-focused-live-preference', 'sub');
	await expect(focus).toHaveAttribute('data-focused-live-selected-variant', 'sub');
	await expect
		.poll(() =>
			page.evaluate(() => {
				const value = localStorage.getItem('keeppeek-playback-preferences');
				return value ? JSON.parse(value) : null;
			})
		)
		.toMatchObject({
			version: 1,
			focusedLive: { cameras: { 'front-door': 'sub', porch: 'high' } }
		});

	await page.goto('/');
	for (const video of await page.locator('[data-peek-wall] video').all()) {
		await presentMockVideoFrame(video);
	}
	await expect(page.locator('[data-peek-wall] [data-camera-id="front-door"]')).toHaveAttribute(
		'data-requested-quality',
		'low'
	);
});

test('overlays focus controls without clipping wide or narrow stream stages', async ({ page }) => {
	await page.setViewportSize({ width: 1188, height: 624 });
	const cameras = [
		{ id: 'wide', name: 'Wide Camera', resolution: '1920x1080' },
		{ id: 'narrow', name: 'Narrow Camera', resolution: '1024x1536' }
	].map((camera) => ({
		id: camera.id,
		name: camera.name,
		ip: `192.0.2.${camera.id.length}`,
		manufacturer: 'ONVIF',
		model: null,
		firmware_version: null,
		is_reolink: false,
		profiles: [
			{
				name: 'Main',
				stream: 'main' as const,
				encoding: 'h264',
				resolution: camera.resolution,
				framerate: 15
			}
		]
	}));
	await mockControlPeer(page, {
		cameras,
		health: {
			status: 'healthy',
			cameras: cameras.map((camera) => ({
				id: camera.id,
				state: 'healthy',
				lifecycle: 'Connected',
				last_error: null,
				streams: []
			}))
		}
	});

	const geometry = () =>
		page.evaluate(() => {
			const main = document.querySelector<HTMLElement>('[data-shell-main]');
			const options = document.querySelector<HTMLElement>('[data-peek-focus-options]');
			const stage = document.querySelector<HTMLElement>('[data-peek-focus-stage]');
			if (!main || !options || !stage) throw new Error('Focus geometry is unavailable');
			return {
				mainClientHeight: main.clientHeight,
				mainScrollHeight: main.scrollHeight,
				main: main.getBoundingClientRect().toJSON(),
				options: options.getBoundingClientRect().toJSON(),
				stage: stage.getBoundingClientRect().toJSON()
			};
		});

	await page.goto('/viewer?camera=wide');
	let focus = page.getByRole('region', { name: 'Wide Camera focus' });
	await expect(focus).toBeVisible();
	let bounds = await geometry();
	expect(bounds.mainScrollHeight).toBeLessThanOrEqual(bounds.mainClientHeight + 1);
	expect(bounds.options).toEqual(bounds.stage);
	expect(bounds.stage).toEqual(bounds.main);

	await page.goto('/viewer?camera=narrow');
	focus = page.getByRole('region', { name: 'Narrow Camera focus' });
	await expect(focus).toBeVisible();
	bounds = await geometry();
	expect(bounds.mainScrollHeight).toBeLessThanOrEqual(bounds.mainClientHeight + 1);
	expect(bounds.options).toEqual(bounds.stage);
	expect(bounds.stage).toEqual(bounds.main);
	await expect(focus.getByLabel('Camera filmstrip')).toBeVisible();
});

test('names the negotiated first-keyframe wait without rewriting server health', async ({
	page
}) => {
	await mockControlPeer(page, {
		cameras: [
			{
				id: 'alley',
				ip: '192.0.2.40',
				name: 'Alley',
				manufacturer: 'ONVIF',
				model: null,
				firmware_version: null,
				is_reolink: false,
				capabilities: {
					ptz: false,
					audio: false,
					events: false,
					recording: true,
					analytics: false,
					imaging: false,
					two_way_audio: false
				},
				profiles: [
					{
						name: 'Sub',
						stream: 'sub',
						encoding: 'h264',
						resolution: '640x360',
						framerate: 15
					}
				]
			}
		],
		health: {
			status: 'healthy',
			cameras: [
				{
					id: 'alley',
					state: 'healthy',
					lifecycle: 'Connected',
					last_error: null,
					streams: []
				}
			]
		}
	});
	await page.goto('/');

	const state = page.locator('[data-first-frame-state]');
	const tile = page.locator('[data-peek-camera="alley"]');
	const wall = page.locator('[data-peek-wall]');
	await expect(wall).toHaveAttribute('data-peek-wall-state', 'staging');
	await expect(wall.locator('[data-peek-wall-content]')).toHaveCSS('opacity', '0');
	await expect(state).toHaveAttribute('data-first-frame-state', 'waiting');
	await expect(tile).toHaveAttribute('data-peek-camera-state', 'healthy');
	await expect(state).toContainText('Negotiated · waiting for a keyframe');
	await expect(wall).toHaveAttribute('data-peek-wall-state', 'ready');
	await expect(wall).toHaveAttribute('data-peek-wall-reveal', 'timeout');
	await expect(wall.locator('[data-peek-wall-content]')).toHaveCSS('opacity', '1');
	await expect(tile).toHaveAttribute('data-peek-camera-state', 'healthy');
	await expect(state).toHaveAttribute('data-first-frame-state', 'waiting');
	await expect(state).toContainText('CONNECTING');
	await expect(state).not.toContainText('DEGRADED');
});

test('uses a compatible H.264 main on the wall when the substream is H.265', async ({ page }) => {
	await mockControlPeer(page, {
		cameras: [
			{
				id: 'mixed-codec',
				ip: '192.0.2.41',
				name: 'Mixed codec',
				manufacturer: 'ONVIF',
				model: null,
				firmware_version: null,
				is_reolink: false,
				profiles: [
					{
						name: 'Main',
						stream: 'main',
						encoding: 'h264',
						resolution: '3840x2160',
						framerate: 25
					},
					{
						name: 'Sub',
						stream: 'sub',
						encoding: 'h265',
						resolution: '640x360',
						framerate: 15
					}
				]
			}
		],
		health: {
			status: 'healthy',
			cameras: [
				{
					id: 'mixed-codec',
					state: 'healthy',
					lifecycle: 'Connected',
					last_error: null,
					streams: []
				}
			]
		}
	});
	await page.goto('/');

	await expect(page.locator('[data-camera-id="mixed-codec"]')).toHaveAttribute(
		'data-requested-variant',
		'main'
	);
});
