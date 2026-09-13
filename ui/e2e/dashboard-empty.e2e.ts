import { expect, test } from '@playwright/test';
import type { CameraListItem } from '../src/lib/types';
import { mockControlPeer } from './fixtures/control-peer';
import { mixedCameras, mixedHealth } from './fixtures/peek';

for (const width of [1440, 320]) {
	test.describe(`empty Dashboard at ${width}px`, () => {
		test.use({
			viewport: { width, height: 844 },
			isMobile: width === 320,
			hasTouch: width === 320
		});

		test('lets an Administrator reach the first camera wizard by keyboard', async ({
			page
		}, testInfo) => {
			await mockControlPeer(page, { cameras: [], health: { status: 'healthy', cameras: [] } });
			await page.goto('/');
			await expect(page.getByRole('heading', { name: 'No cameras yet' })).toBeVisible();
			await expect(
				page.getByText('Add your first camera to start viewing and recording.', { exact: true })
			).toBeVisible();
			const addCamera = page.getByRole('link', { name: 'Add camera', exact: true });
			await expect(addCamera).toHaveAttribute('href', '/cameras/new');
			await expect(addCamera).toBeInViewport({ ratio: 1 });
			const bounds = await addCamera.boundingBox();
			expect(bounds?.height).toBeGreaterThanOrEqual(44);
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
				width
			);
			await addCamera.focus();
			await page.keyboard.press('Shift+Tab');
			await page.keyboard.press('Tab');
			await expect(addCamera).toBeFocused();
			await testInfo.attach('empty-dashboard', {
				body: await page.screenshot(),
				contentType: 'image/png'
			});
			await page.keyboard.press('Enter');
			await expect(page).toHaveURL(/\/cameras\/new$/);
			await expect(
				page.getByRole('heading', {
					name: width === 320 ? 'Add a camera' : 'Add camera',
					exact: true
				})
			).toBeVisible();
			await expect(page.getByLabel('Address or RTSP URL')).toBeVisible();
		});

		test('asks a User to contact an administrator without offering camera setup', async ({
			page
		}, testInfo) => {
			await mockControlPeer(page, {
				accessRole: 'user',
				accessLocal: false,
				cameras: [],
				health: { status: 'healthy', cameras: [] }
			});
			await page.goto('/');
			await expect(page.getByRole('heading', { name: 'No cameras available' })).toBeVisible();
			await expect(
				page.getByText('Ask your administrator for access.', { exact: true })
			).toBeVisible();
			await expect(page.getByRole('link', { name: 'Add camera', exact: true })).toHaveCount(0);
			await expect(page.getByRole('link', { name: 'Cameras', exact: true })).toHaveCount(0);
			await expect(page.getByRole('heading', { name: 'No cameras yet' })).toHaveCount(0);
			await testInfo.attach('restricted-dashboard', {
				body: await page.screenshot(),
				contentType: 'image/png'
			});
		});
	});
}

test('waits for the Dashboard inventory to load before offering first camera setup', async ({
	page
}, testInfo) => {
	let releaseHealth!: () => void;
	const healthGate = new Promise<void>((resolve) => (releaseHealth = resolve));
	await mockControlPeer(page, { cameras: [], health: { cameras: [] }, healthGate });
	try {
		await page.goto('/');
		await expect(page.getByRole('status', { name: 'Loading live view' })).toBeVisible();
		await expect(page.getByRole('link', { name: 'Add camera', exact: true })).toHaveCount(0);
		await expect(page.getByRole('heading', { name: 'No cameras yet' })).toHaveCount(0);
		await testInfo.attach('loading-dashboard', {
			body: await page.screenshot(),
			contentType: 'image/png'
		});
		releaseHealth();
		await expect(page.getByRole('heading', { name: 'No cameras yet' })).toBeVisible();
		await expect(page.getByRole('status', { name: 'Loading live view' })).toHaveCount(0);
		await expect(page.getByRole('link', { name: 'Add camera', exact: true })).toBeVisible();
	} finally {
		releaseHealth();
	}
});

test('removes the first camera action when the Dashboard gains configured cameras', async ({
	page
}) => {
	const cameras: CameraListItem[] = [];
	const controls = await mockControlPeer(page, { cameras, health: mixedHealth });
	await page.goto('/');
	await page.getByRole('link', { name: 'Add camera', exact: true }).click();
	await expect(page).toHaveURL(/\/cameras\/new$/);
	cameras.push(...mixedCameras);
	await controls.publishCapabilities([]);
	await page.getByRole('link', { name: 'Dashboard', exact: true }).click();
	await expect(page.locator('[data-peek-camera]')).toHaveCount(mixedCameras.length);
	await expect(page.locator('[data-peek-camera="front-door"]')).toBeVisible();
	await expect(page.getByRole('link', { name: 'Add camera', exact: true })).toHaveCount(0);
	await expect(page.getByRole('heading', { name: 'No cameras yet' })).toHaveCount(0);
});
