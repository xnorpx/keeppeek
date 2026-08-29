import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import { mockRecordingCoverage } from './fixtures/recording-coverage';

test('shows fleet recording state, camera evidence, filters, and gap playback links', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	await mockRecordingCoverage(page);
	await mockControlPeer(page);

	await page.goto('/recordings');

	await expect(page).toHaveTitle('Recording integrity - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Recording integrity' })).toBeVisible();
	await expect(page.locator('[data-recording-camera]')).toHaveCount(3);
	await expect(page.getByText('Recording degraded', { exact: true })).toBeVisible();
	await expect(page.getByText('Paused by policy', { exact: true })).toBeVisible();
	await expect(page.locator('[data-recording-camera-detail]')).toContainText('Front Door');
	await expect(page.getByLabel('Fleet recording summary')).toContainText('NOT CONFIGURED');
	await expect(page.getByText(/HEADROOM 800 GB · Selected finalized playable/)).toBeVisible();
	await expect(page.locator('[data-recording-camera-detail]')).toContainText('340 MB / DAY');
	await expect(page.locator('[data-recording-coverage-strip]')).toHaveAttribute(
		'aria-label',
		/main stream has .* playable indexed coverage and 1 gaps/
	);
	await expect(page.locator('[data-recording-gap]')).toContainText('Writer failure');
	await expect(page.locator('[data-recording-gap]')).toContainText('Open');
	await expect(page.getByRole('link', { name: 'Open relevant logs' })).toHaveAttribute(
		'href',
		'/settings/logs'
	);
	await page.getByRole('button', { name: '7d', exact: true }).click();
	await expect(page.locator('[data-recording-coverage-strip]')).toContainText(
		'1h buckets · exact totals retained'
	);
	await page.getByRole('button', { name: '24h', exact: true }).click();

	await page.getByLabel('Recording state').selectOption('healthy');
	await expect(page.locator('[data-recording-camera]')).toHaveCount(1);
	await expect(page.getByText('Recording healthy', { exact: true })).toBeVisible();
	await page.getByLabel('Recording state').selectOption('');
	await expect(page.locator('[data-recording-camera]')).toHaveCount(3);
	await page.getByLabel('Camera group').selectOption('Interior');
	await expect(page.locator('[data-recording-camera]')).toHaveCount(1);
	await expect(page.locator('[data-recording-camera="camera-002"]')).toBeVisible();
	await page.getByLabel('Camera group').selectOption('');
	await expect(page.locator('[data-recording-camera]')).toHaveCount(3);
	await page.getByLabel('Minimum camera gap').selectOption('60000');
	await expect(page.locator('[data-recording-camera]')).toHaveCount(1);
	await expect(page.locator('[data-recording-camera="front-door"]')).toBeVisible();
	await page.getByLabel('Minimum camera gap').selectOption('0');
	await expect(page.locator('[data-recording-camera]')).toHaveCount(3);

	const playbackLink = page.getByRole('link', { name: 'Open footage before gap' });
	await expect(playbackLink).toHaveAttribute(
		'href',
		/\/keep\?camera=front-door&stream=main&at=\d+/
	);
	expect(browserErrors).toEqual([]);
	await playbackLink.click();
	await expect(page).toHaveURL(/\/keep\?camera=front-door&stream=main&at=\d+/);
});

test('pages a 127-camera fleet without growing the recording dashboard DOM', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockRecordingCoverage(page, 127);
	await mockControlPeer(page);

	await page.goto('/recordings');

	await expect(page.locator('[data-recording-camera]')).toHaveCount(25);
	await expect(page.getByText('PAGE 1 · 127 CAMERAS')).toBeVisible();
	await page.getByRole('button', { name: 'Next camera page' }).click();
	await expect(page.getByText('PAGE 2 · 127 CAMERAS')).toBeVisible();
	await expect(page.locator('[data-recording-camera]')).toHaveCount(25);
	await expect(page.locator('[data-recording-camera="camera-026"]')).toBeVisible();
	await expect(page.locator('[data-recording-camera="front-door"]')).toHaveCount(0);
	await expect
		.poll(() =>
			page
				.locator('[data-recording-dashboard]')
				.evaluate((dashboard) => dashboard.querySelectorAll('*').length)
		)
		.toBeLessThan(1_500);
});

test('keeps labeled recording evidence and gap actions usable on mobile', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockRecordingCoverage(page);
	await mockControlPeer(page);

	await page.goto('/recordings');

	await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toContainText('Keep');
	await expect(page.getByText('Recording degraded', { exact: true })).toBeVisible();
	await expect(page.locator('[data-recording-gap]')).toContainText('Writer failure');
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	const gapAction = page.getByRole('link', { name: 'Open footage before gap' });
	await expect
		.poll(async () => {
			const bounds = await gapAction.boundingBox();
			return bounds ? [Math.round(bounds.width), Math.round(bounds.height)] : null;
		})
		.toEqual([44, 44]);
});
