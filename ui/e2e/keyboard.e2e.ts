import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import { mockCameraFleet } from './fixtures/camera-fleet';
import { eventDate, mockEvents } from './fixtures/events';
import { keepModeCameras, keepModeDate, mockKeepModes } from './fixtures/keep-modes';
import { mockMixedHealth } from './fixtures/peek';

async function waitForKeyboard(page: import('@playwright/test').Page): Promise<void> {
	await expect(page.locator('[data-keyboard-ready]')).toHaveAttribute(
		'data-keyboard-ready',
		'true'
	);
}

test('uses one roving rail tab stop with the Board 32 focus outline', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockControlPeer(page, { cameras: [], health: { status: 'healthy', cameras: [] } });
	await page.goto('/');
	await waitForKeyboard(page);

	const railLinks = page.locator('[data-shell-rail-link]');
	await expect(railLinks).toHaveCount(6);
	expect(
		await railLinks.evaluateAll((links) => links.map((link) => link.getAttribute('tabindex')))
	).toEqual(['0', '-1', '-1', '-1', '-1', '-1']);
	await railLinks.nth(0).focus();
	await page.keyboard.press('ArrowDown');
	await expect(railLinks.nth(1)).toBeFocused();
	await expect(railLinks.nth(1)).toHaveCSS('outline-width', '2px');
	await expect(railLinks.nth(1)).toHaveCSS('outline-offset', '2px');
	await page.keyboard.press('ArrowUp');
	await expect(railLinks.nth(0)).toBeFocused();
});

test('opens Board 32 keyboard help and returns focus to its invoker', async ({ page }) => {
	await mockControlPeer(page, { cameras: [], health: { status: 'healthy', cameras: [] } });
	await page.goto('/');
	await waitForKeyboard(page);

	const invoker = page.getByRole('button', { name: /switch to (light|dark) theme/i });
	await invoker.focus();
	await page.keyboard.press('Shift+/');

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Shortcuts and focus' })).toBeVisible();
	await expect(dialog).toContainText('Typing in a field always wins');
	await expect(dialog).toContainText('No shortcut is destructive');
	await expect(dialog.getByText('Anywhere', { exact: true })).toBeVisible();
	await expect(dialog.getByText('This screen', { exact: true })).toBeVisible();
	await expect(dialog).toContainText('Move focus across the camera grid');
	await expect(dialog).toContainText('Focus the selected camera or return to the grid');
	await expect(dialog).toContainText('Open the focused camera');
	await expect(dialog).not.toContainText('Rewind from the focused live view control');
	await expect(dialog).not.toContainText('Switch to a saved layout');
	await expect
		.poll(() => page.evaluate(() => document.activeElement?.closest('dialog') !== null))
		.toBe(true);

	await page.keyboard.press('Escape');
	await expect(dialog).toBeHidden();
	await expect(invoker).toBeFocused();
});

test('uses fixed G chords and lets typing beat single-letter navigation', async ({ page }) => {
	const cameras = keepModeCameras(2);
	await mockControlPeer(page, { cameras, health: { status: 'healthy', cameras: [] } });
	await page.goto('/cameras');
	await waitForKeyboard(page);

	await page.keyboard.press('/');
	const search = page.getByRole('searchbox');
	await expect(search).toBeFocused();
	await page.keyboard.type('gp');
	await expect(search).toHaveValue('gp');
	await expect(page).toHaveURL(/\/cameras$/);

	await search.evaluate((input: HTMLInputElement) => input.blur());
	await page.keyboard.press('g');
	await expect(page.locator('[data-keyboard-navigation-chord]')).toBeVisible();
	await page.keyboard.press('p');
	await expect(page).toHaveURL('/');
	await expect(page.locator('[data-keyboard-navigation-chord]')).toHaveCount(0);
});

test('finds a real camera through the Board 32 command palette', async ({ page }) => {
	const cameras = keepModeCameras(2);
	await mockControlPeer(page, { cameras, health: { status: 'healthy', cameras: [] } });
	await page.goto('/');
	await waitForKeyboard(page);

	await page.keyboard.press('Control+k');
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	const search = page.getByRole('searchbox', { name: 'Find a camera or setting' });
	await expect(search).toBeFocused();
	await search.fill('Front Door');
	await expect(page.getByRole('option', { name: /Front Door/ })).toBeVisible();
	await search.press('Enter');

	await expect(page).toHaveURL('/camera?camera=front-door');
	await expect(dialog).toBeHidden();
});

test('moves Peek focus spatially and keeps Enter distinct from fullscreen', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockMixedHealth(page);
	await page.goto('/');
	await waitForKeyboard(page);

	const frontDoor = page.locator('[data-peek-focus="front-door"]');
	const porch = page.locator('[data-peek-focus="porch"]');
	await frontDoor.focus();
	await page.keyboard.press('ArrowRight');
	await expect(porch).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL('/camera?camera=porch');

	await page.goto('/');
	await waitForKeyboard(page);
	await page.locator('[data-peek-focus="front-door"]').focus();
	await page.keyboard.press('f');
	const focus = page.getByRole('region', { name: 'Front Door focus' });
	await expect(focus).toBeVisible();
	const primaryView = focus.locator('[data-peek-focus-history]');
	const filmstrip = focus.getByLabel('Other cameras');
	await expect(filmstrip).toBeVisible();
	const firstFilmstripItem = filmstrip.locator('article').first();
	const [primaryBox, filmstripBox, firstFilmstripItemBox] = await Promise.all([
		primaryView.boundingBox(),
		filmstrip.boundingBox(),
		firstFilmstripItem.boundingBox()
	]);
	expect(primaryBox).not.toBeNull();
	expect(filmstripBox).not.toBeNull();
	expect(firstFilmstripItemBox).not.toBeNull();
	expect(filmstripBox!.y).toBeGreaterThan(primaryBox!.y + primaryBox!.height - 1);
	expect(firstFilmstripItemBox!.width).toBeLessThan(primaryBox!.width / 2);
	expect(firstFilmstripItemBox!.height).toBeLessThan(primaryBox!.height / 2);
	await page.keyboard.press('f');
	await expect(page.locator('[data-peek-focus="front-door"]')).toBeFocused();
});

test('controls Keep transport, exact frames, live follow, and export range from the keyboard', async ({
	page
}) => {
	await mockKeepModes(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}`);
	await waitForKeyboard(page);

	const player = page.locator('[data-keep-player]');
	const video = player.locator('video');
	await expect(video).toBeVisible();
	await video.focus();

	await page.keyboard.press('l');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-direction', '1');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-speed', '1');
	await expect(player).toHaveAttribute('data-keyboard-playing', 'true');
	await page.keyboard.press('l');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-speed', '2');
	await expect(video).toHaveJSProperty('playbackRate', 2);

	await page.keyboard.press('k');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-direction', '0');
	await expect(player).toHaveAttribute('data-keyboard-playing', 'false');
	await page.keyboard.press('j');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-direction', '-1');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-speed', '1');
	await page.keyboard.press(' ');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-direction', '0');
	await page.keyboard.press(' ');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-direction', '1');
	await expect(player).toHaveAttribute('data-keyboard-shuttle-speed', '1');

	await page.keyboard.press('k');
	const beforeFrame = Number(await player.getAttribute('data-recording-playhead-ms'));
	await page.keyboard.press('ArrowRight');
	await expect
		.poll(async () => Number(await player.getAttribute('data-recording-playhead-ms')))
		.toBe(beforeFrame + 40);
	await page.keyboard.press('[');
	await page.keyboard.press('ArrowRight');
	await page.keyboard.press(']');

	const timeline = page.getByRole('region', { name: 'Recording timeline', exact: true });
	const viewport = page.getByRole('region', { name: 'Recording timeline pan viewport' });
	await viewport.dispatchEvent('wheel', { deltaY: 100 });
	await expect(timeline).toHaveAttribute('data-timeline-following', 'false');
	await video.focus();
	await page.keyboard.press('Home');
	await expect(timeline).toHaveAttribute('data-timeline-following', 'true');
	await expect(player).toHaveAttribute('data-keyboard-playing', 'true');

	await page.getByRole('button', { name: 'Export', exact: true }).click();
	const exportPanel = page.locator('[data-keep-export]');
	await expect(exportPanel).toHaveAttribute('data-export-start-ms', String(beforeFrame + 40));
	await expect(exportPanel).toHaveAttribute('data-export-end-ms', String(beforeFrame + 80));
});

test('moves Event card focus and opens only the selected card with Enter', async ({ page }) => {
	await mockEvents(page);
	await page.goto(`/events?date=${eventDate}`);
	await waitForKeyboard(page);

	const cards = page.locator('[data-event-card]');
	await expect(cards).toHaveCount(5);
	await cards.nth(0).focus();
	await page.keyboard.press('ArrowDown');
	await expect(cards.nth(1)).toBeFocused();
	const selectedKey = await cards.nth(1).getAttribute('data-event-card');
	await page.keyboard.press('Enter');
	await expect(page.locator('[data-event-detail]')).toHaveAttribute(
		'data-event-detail',
		selectedKey!
	);
});

test('moves through the virtualized fleet and toggles bulk selection with Space', async ({
	page
}) => {
	await mockCameraFleet(page);
	await page.goto('/cameras');
	await waitForKeyboard(page);

	const frontDoor = page.locator('[data-fleet-focus="front-door"]');
	const porch = page.locator('[data-fleet-focus="porch"]');
	await frontDoor.focus();
	await page.keyboard.press('ArrowDown');
	await expect(porch).toBeFocused();
	await page.keyboard.press(' ');
	await expect(page.getByRole('checkbox', { name: 'Select Porch' })).toBeChecked();
	await expect(page.getByText('1 selected', { exact: true })).toBeVisible();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL('/camera?camera=porch');
});

test('saves only the active Settings draft with Control+S', async ({ page }) => {
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
	const updatedConfig = {
		host: '0.0.0.0',
		port: 3201,
		camera_count: 0,
		storage,
		recording_estimate: recordingEstimate
	};
	const controls = await mockControlPeer(page, {
		runtimeConfiguration: { ...updatedConfig, port: 3000 },
		runtimeUpdateResult: { config: updatedConfig, restart_required: false }
	});
	await page.goto('/settings');
	await waitForKeyboard(page);

	await page.getByRole('button', { name: 'Edit server' }).click();
	await page.getByLabel('Port').fill('3201');
	await page.keyboard.press('Control+s');
	await expect(page.getByText('Server and storage settings saved.', { exact: true })).toBeVisible();
	expect(controls.runtimeUpdates).toEqual([
		{
			host: '0.0.0.0',
			port: 3201,
			storage,
			move_existing_recordings: false
		}
	]);
});
