import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer, type MockControlPeerOptions } from './fixtures/control-peer';
import { defaultPeekWallPreferences, wallDisplayToWire } from '../src/lib/peek-wall-preferences';
import { mixedCameras, mixedHealth } from './fixtures/peek';
import { presentMockVideoFrame } from './fixtures/media';

const cameras = mixedCameras.slice(0, 3).map((camera) => ({
	...camera,
	profiles: [
		{ name: 'Sub', stream: 'sub' as const, encoding: 'h264', resolution: '640x360', framerate: 15 }
	]
}));

function mockWall(page: Page, options: MockControlPeerOptions = {}) {
	const sources = options.cameras ?? cameras;
	const tiles = sources.map((camera, index) => ({
		camera_id: camera.id,
		column: index === 0 ? 1 : 9,
		row: index === 2 ? 7 : 1,
		column_span: sources.length === 1 ? 12 : index === 0 ? 8 : 4,
		row_span: index === 0 ? 12 : 6,
		pinned: index === 0
	}));
	return mockControlPeer(page, {
		cameras: sources,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		peekLayoutRegistry: {
			schema_version: 1,
			active_layout_id: 'default',
			layouts: [
				{
					id: 'default',
					name: 'All cameras',
					scope: 'shared',
					owner_id: 'server',
					activity_focus: true,
					display: wallDisplayToWire(defaultPeekWallPreferences()),
					tiles
				},
				{
					id: 'phone',
					name: 'Phone',
					scope: 'shared',
					owner_id: 'server',
					activity_focus: true,
					display: wallDisplayToWire({
						...defaultPeekWallPreferences(),
						gapPx: 12,
						cornerRadiusPx: 16
					}),
					tiles
				}
			]
		},
		...options
	});
}

test('saves appearance on the selected server dashboard and discards previews independently', async ({
	page
}) => {
	await page.addInitScript(
		(preferences) => {
			localStorage.setItem('keeppeek-peek-wall-preferences', JSON.stringify(preferences));
		},
		{ ...defaultPeekWallPreferences(), gapPx: 24, cornerRadiusPx: 24 }
	);
	const controls = await mockWall(page);
	await page.goto('/');
	await page.getByRole('button', { name: 'Wall display settings' }).click();
	await expect(page.getByRole('spinbutton', { name: 'Gap (px)' })).toHaveValue('10');
	await page.getByRole('radio', { name: 'Hairline', exact: true }).check();
	const wall = page.locator('[data-peek-wall-content]');
	await expect(wall).toHaveCSS('gap', '2px');
	await expect(page.locator('[data-peek-camera]').first()).toHaveCSS('border-radius', '0px');
	expect(controls.peekLayoutUpdates).toEqual([]);
	await page.getByRole('spinbutton', { name: 'Gap (px)' }).fill('7');
	await page.getByRole('spinbutton', { name: 'Corner radius (px)' }).fill('18');
	await page.getByRole('button', { name: 'Save display settings' }).click();
	await expect.poll(() => controls.peekLayoutUpdates.length).toBe(1);
	expect(controls.peekLayoutUpdates[0].layouts).toEqual(
		expect.arrayContaining([
			expect.objectContaining({
				id: 'default',
				display: expect.objectContaining({ gap_px: 7, corner_radius_px: 18 })
			}),
			expect.objectContaining({
				id: 'phone',
				display: expect.objectContaining({ gap_px: 12, corner_radius_px: 16 })
			})
		])
	);
	await page.reload();
	await expect(wall).toHaveCSS('gap', '7px');
	await expect(page.locator('[data-peek-camera]').first()).toHaveCSS('border-radius', '18px');
	await page.getByRole('button', { name: 'Choose dashboard, All cameras' }).click();
	await page.getByRole('menuitemradio', { name: 'Phone' }).click();
	await expect(wall).toHaveCSS('gap', '12px');
	await expect(page.locator('[data-peek-camera]').first()).toHaveCSS('border-radius', '16px');
	await page.getByRole('button', { name: 'Wall display settings' }).click();
	await page.getByRole('radio', { name: 'Flush', exact: true }).check();
	await expect(wall).toHaveCSS('gap', '0px');
	await page.getByRole('button', { name: 'Discard changes' }).click();
	await expect(wall).toHaveCSS('gap', '12px');
	await expect(page.getByRole('spinbutton', { name: 'Corner radius (px)' })).toHaveValue('16');
});

test('retains a rejected dashboard preview until it is explicitly discarded', async ({ page }) => {
	await mockWall(page, {
		peekLayoutSaveError: 'Dashboard changed on the server. Reload before saving.'
	});
	await page.goto('/');
	await page.getByRole('button', { name: 'Wall display settings' }).click();
	await page.getByRole('spinbutton', { name: 'Gap (px)' }).fill('7');
	await page.getByRole('button', { name: 'Save display settings' }).click();
	await expect(page.getByRole('alert')).toContainText('Dashboard changed on the server');
	await expect(page.locator('[data-peek-wall-content]')).toHaveCSS('gap', '7px');
	await page.getByRole('button', { name: 'Discard changes' }).click();
	await expect(page.locator('[data-peek-wall-content]')).toHaveCSS('gap', '10px');
	await expect(page.getByRole('alert')).toHaveCount(0);
});

test('shared display settings are read-only for a User', async ({ page }) => {
	const controls = await mockWall(page, { accessRole: 'user', accessLocal: false });
	await page.goto('/');
	await page.getByRole('button', { name: 'Wall display settings' }).click();
	await expect(page.getByText('Read-only dashboard')).toBeVisible();
	await expect(page.getByRole('slider', { name: 'Gap', exact: true })).toBeDisabled();
	await expect(page.getByRole('radio', { name: 'Continuous', exact: true })).toBeDisabled();
	expect(controls.peekLayoutUpdates).toEqual([]);
});

test('wall previews select the lowest compatible quality rank instead of a stream name', async ({
	page
}) => {
	const source = cameras[0];
	const controls = await mockWall(page, {
		cameras: [
			{
				...source,
				profiles: [
					{ ...source.profiles[0], name: 'Main', stream: 'main', quality_rank: 1 },
					{ ...source.profiles[0], quality_rank: 2 }
				]
			}
		],
		health: mixedHealth
	});
	await page.goto('/');
	await expect
		.poll(() => controls.mediaSubscriptions.map((subscription) => subscription.variantId))
		.toEqual(['main']);
});

test('wall does not request a codec the browser reports as unsupported', async ({ page }) => {
	const source = cameras[0];
	const controls = await mockWall(page, {
		cameras: [{ ...source, profiles: [{ ...source.profiles[0], encoding: 'h265' }] }],
		health: mixedHealth
	});
	await page.addInitScript(() => {
		Object.defineProperty(RTCRtpReceiver, 'getCapabilities', {
			configurable: true,
			value: () => ({ codecs: [{ mimeType: 'video/h264' }], headerExtensions: [] })
		});
	});
	await page.goto('/');
	await expect(page.getByText('No browser-compatible stream')).toBeVisible();
	expect(controls.mediaSubscriptions).toEqual([]);
});

async function expectSeparateTileEvidence(page: Page): Promise<void> {
	const overlaps = await page.locator('[data-peek-camera]').evaluateAll((tiles) =>
		tiles.flatMap((tile) => {
			const regions = [
				...tile.querySelectorAll(
					'[data-live-frame-freshness], [data-peek-camera-region="evidence"], [data-peek-camera-label]'
				)
			].map((element) => element.getBoundingClientRect());
			return regions.flatMap((bounds, index) =>
				regions
					.slice(index + 1)
					.flatMap((other) =>
						bounds.left < other.right &&
						bounds.right > other.left &&
						bounds.top < other.bottom &&
						bounds.bottom > other.top
							? [tile.getAttribute('data-peek-camera')]
							: []
					)
			);
		})
	);
	expect(overlaps).toEqual([]);
}

test('capacity and crop labels remain separate from mobile camera controls', async ({
	page
}, testInfo) => {
	await page.setViewportSize({ width: 320, height: 844 });
	await mockWall(page);
	await page.goto('/');
	await expect(page.locator('[data-peek-camera]')).toHaveCount(3);
	for (const video of await page.locator('[data-peek-camera] video').all()) {
		await presentMockVideoFrame(video);
	}
	await page.getByRole('button', { name: 'Wall display settings' }).click();
	await page.getByRole('radio', { name: 'Cover (cropped)' }).check();
	await page.getByRole('spinbutton', { name: 'Stream limit' }).fill('1');
	await page.keyboard.press('Tab');
	await page.keyboard.press('Escape');
	await expect(page.locator('[data-peek-admission="capacity"]')).toHaveCount(2);
	await expectSeparateTileEvidence(page);
	await page.screenshot({ path: testInfo.outputPath('wall-capacity-320.png') });
});

for (const width of [320, 390, 768, 1440]) {
	test(`wall display preferences and geometry at ${width}px`, async ({ page }, testInfo) => {
		await page.setViewportSize({ width, height: 900 });
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		const controls = await mockWall(page);
		await page.goto('/');
		await expect(page.locator('[data-peek-camera]')).toHaveCount(3);
		for (const video of await page.locator('[data-peek-camera] video').all()) {
			await presentMockVideoFrame(video);
		}
		await expect(page.locator('[data-peek-wall]')).toHaveAttribute('data-peek-wall-state', 'ready');
		await expect.poll(() => controls.mediaSubscriptions.length).toBeGreaterThan(0);
		const subscriptions = controls.mediaSubscriptions.length;
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await expect(page.locator('[data-popover-content]')).toBeInViewport({ ratio: 1 });
		await expect(page.getByRole('button', { name: 'Save display settings' })).toBeInViewport({
			ratio: 1
		});
		await expect(page.getByRole('button', { name: 'Discard changes', exact: true })).toBeInViewport(
			{
				ratio: 1
			}
		);
		await page.getByRole('radio', { name: '4:3', exact: true }).check();
		await page.getByRole('radio', { name: 'Cover (cropped)' }).check();
		await page.keyboard.press('Escape');
		const firstTile = page.locator('[data-peek-camera="front-door"]');
		await expect(firstTile).toHaveAttribute('data-peek-tile-shape', '4:3');
		await expect(firstTile.locator('video')).toHaveCSS('object-fit', 'cover');
		const frame = firstTile.locator('[data-live-media-frame]');
		await expect
			.poll(async () => {
				const bounds = await frame.boundingBox();
				return bounds ? Math.abs(bounds.width / bounds.height - 4 / 3) : Infinity;
			})
			.toBeLessThan(0.01);
		expect(controls.mediaSubscriptions).toHaveLength(subscriptions);
		expect(controls.peekLayoutUpdates).toEqual([]);
		const geometry = await page.evaluate(() => {
			const settings = document
				.querySelector<HTMLElement>('[data-peek-wall-settings]')!
				.getBoundingClientRect();
			const selector = document
				.querySelector<HTMLElement>('[data-peek-dashboard-switcher]')!
				.getBoundingClientRect();
			const frame = document.querySelector<HTMLElement>('[data-wall-settings-frame]')!;
			const selectorElement = document.querySelector<HTMLElement>(
				'[data-peek-dashboard-switcher]'
			)!;
			const overlappingCameraControls = [
				...document.querySelectorAll<HTMLElement>('[data-peek-camera] [data-peek-camera-label]')
			].filter((control) => {
				const bounds = control.getBoundingClientRect();
				return (
					bounds.left < settings.right &&
					bounds.right > settings.left &&
					bounds.top < settings.bottom &&
					bounds.bottom > settings.top
				);
			}).length;
			return {
				buttonFrameHeight: frame.getBoundingClientRect().height,
				selectorHeight: selector.height,
				buttonRadius: getComputedStyle(frame).borderRadius,
				selectorRadius: getComputedStyle(selectorElement).borderRadius,
				overlappingCameraControls,
				settingsRight: settings.right,
				selectorRight: selector.right,
				settingsLeft: settings.left,
				pageWidth: document.documentElement.clientWidth,
				scrollWidth: document.documentElement.scrollWidth
			};
		});
		expect(geometry.settingsRight).toBeLessThanOrEqual(width);
		expect(geometry.buttonFrameHeight).toBe(geometry.selectorHeight);
		expect(geometry.buttonRadius).toBe(geometry.selectorRadius);
		expect(geometry.overlappingCameraControls).toBe(0);
		expect(geometry.selectorRight).toBeLessThanOrEqual(geometry.settingsLeft);
		expect(geometry.scrollWidth).toBeLessThanOrEqual(geometry.pageWidth);
		await expectSeparateTileEvidence(page);
		await page.screenshot({ path: testInfo.outputPath(`wall-${width}.png`) });
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await page.getByRole('button', { name: 'Save display settings' }).click();
		await expect.poll(() => controls.peekLayoutUpdates.length).toBe(1);
		await page.reload();
		await expect(firstTile).toHaveAttribute('data-peek-tile-shape', '4:3');
		await expect(firstTile).toHaveAttribute('data-peek-media-fit', 'cover');
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await page.getByRole('radio', { name: 'Continuous', exact: true }).check();
		await expect(page.locator('[data-peek-wall]')).toHaveAttribute(
			'data-streaming-mode',
			'continuous'
		);
		await page.getByRole('button', { name: 'Reset wall settings' }).click();
		await expect(firstTile).toHaveAttribute('data-peek-tile-shape', '16:9');
		await expect(firstTile).toHaveAttribute('data-peek-media-fit', 'contain');
		await expect(page.locator('[data-peek-wall]')).toHaveAttribute('data-streaming-mode', 'smart');
		expect(errors).toEqual([]);
	});
}

test('wake lock follows user intent, visibility, and navigation without changing subscriptions', async ({
	page
}) => {
	await page.addInitScript(() => {
		const counts = { requests: 0, releases: 0 };
		Object.assign(window, { __peekWakeCounts: counts });
		Object.defineProperty(navigator, 'wakeLock', {
			configurable: true,
			value: {
				async request() {
					counts.requests += 1;
					const sentinel = new EventTarget() as EventTarget & {
						released: boolean;
						release(): Promise<void>;
					};
					sentinel.released = false;
					sentinel.release = async () => {
						if (sentinel.released) return;
						counts.releases += 1;
						sentinel.released = true;
						sentinel.dispatchEvent(new Event('release'));
					};
					return sentinel;
				}
			}
		});
	});
	const controls = await mockWall(page);
	const counts = () =>
		page.evaluate(() => {
			const snapshot = (
				window as Window & { __peekWakeCounts?: { requests: number; releases: number } }
			).__peekWakeCounts;
			if (!snapshot) throw new Error('Wake-lock fixture is unavailable');
			return snapshot;
		});
	await page.goto('/');
	await page.getByRole('button', { name: 'Wall display settings' }).click();
	expect(await counts()).toEqual({ requests: 0, releases: 0 });
	await expect.poll(() => controls.mediaSubscriptions.length).toBeGreaterThan(0);
	const subscriptions = controls.mediaSubscriptions.length;
	await page.getByRole('switch', { name: 'Keep display awake' }).check();
	await expect(page.locator('[data-wake-lock-state]')).toHaveAttribute(
		'data-wake-lock-state',
		'active'
	);
	expect(await counts()).toEqual({ requests: 1, releases: 0 });
	expect(controls.mediaSubscriptions).toHaveLength(subscriptions);
	await page.evaluate(() => {
		Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect.poll(counts).toEqual({ requests: 1, releases: 1 });
	await expect.poll(() => controls.mediaUnsubscriptions.length).toBeGreaterThan(0);
	await page.evaluate(() => {
		Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect.poll(counts).toEqual({ requests: 2, releases: 1 });
	await page.keyboard.press('Escape');
	await page.getByRole('link', { name: 'Keep', exact: true }).click();
	await expect.poll(counts).toEqual({ requests: 2, releases: 2 });
});
