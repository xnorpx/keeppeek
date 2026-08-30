import { readFile } from 'node:fs/promises';
import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import { mixedCameras, mixedHealth, mockMixedHealth } from './fixtures/peek';

const settingsStorage = {
	medium_term_path: '/recordings/medium',
	long_term_path: '/recordings/long',
	recording_catalog_path: '/recordings/long/recordings.db',
	event_thumbnail_path: '/recordings/long/.event-thumbnails',
	event_thumbnail_max_mb: 1024,
	short_term_secs: 120,
	medium_term_secs: 1800,
	flush_interval_secs: 60,
	write_buffer_bytes: 8192,
	long_term_max_gb: 0,
	minimum_free_gb: 10,
	maximum_used_percent: null,
	warning_free_gb: 20,
	critical_free_gb: 10,
	cleanup_hysteresis_gb: 5
};

const settingsRuntimeConfiguration = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: mixedCameras.length,
	storage: settingsStorage,
	recording_estimate: {
		estimated_bitrate_bps: 8_576_000,
		bytes_per_day: 92_620_800_000,
		known_streams: 2,
		unknown_streams: 0,
		estimated_retention_days: 2
	}
};

function editableDashboardRegistry() {
	return {
		schema_version: 1,
		active_layout_id: 'front-of-house',
		layouts: [
			{
				id: 'default',
				name: 'All cameras',
				scope: 'shared',
				owner_id: 'server',
				audience: { everyone: true, credential_ids: [] },
				activity_focus: true,
				tiles: mixedCameras.map((camera, index) => ({
					camera_id: camera.id,
					column: (index % 2) * 6 + 1,
					row: Math.floor(index / 2) * 6 + 1,
					column_span: 6,
					row_span: 6,
					pinned: index === 0
				}))
			},
			{
				id: 'front-of-house',
				name: 'Front of house',
				scope: 'shared',
				owner_id: 'server',
				audience: { everyone: true, credential_ids: [] },
				activity_focus: true,
				tiles: [
					{
						camera_id: 'front-door',
						column: 1,
						row: 1,
						column_span: 8,
						row_span: 12,
						pinned: true
					},
					{
						camera_id: 'porch',
						column: 9,
						row: 1,
						column_span: 4,
						row_span: 4,
						pinned: false
					},
					{
						camera_id: 'alley',
						column: 9,
						row: 9,
						column_span: 4,
						row_span: 4,
						pinned: false
					}
				]
			}
		]
	};
}

test('edits the 12-column dashboard grid from Settings', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	const controls = await mockControlPeer(page, {
		cameras: mixedCameras,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		runtimeConfiguration: settingsRuntimeConfiguration,
		peekLayoutRegistry: editableDashboardRegistry()
	});

	await page.goto('/settings#dashboards');
	await page
		.getByRole('region', { name: 'Dashboards' })
		.getByRole('button', { name: 'Edit grid' })
		.click();

	await expect(page.locator('[data-peek-layout-editor]')).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Editing “Front of house”' })).toBeVisible();
	await expect(page.getByText('12-COL SNAP', { exact: true })).toBeVisible();
	await expect(page.locator('[data-peek-layout-item]')).toHaveCount(3);
	await expect(page.locator('[data-peek-layout-item="front-door"]')).toHaveAttribute(
		'data-layout-column-span',
		'8'
	);

	const porchTile = page.locator('[data-peek-layout-item="porch"]');
	const porchButton = page.getByRole('button', { name: 'Select Porch layout tile' });
	const canvasBounds = await page.locator('[data-peek-layout-canvas]').boundingBox();
	const porchBounds = await porchButton.boundingBox();
	expect(canvasBounds).not.toBeNull();
	expect(porchBounds).not.toBeNull();
	if (canvasBounds === null || porchBounds === null)
		throw new Error('Layout geometry is unavailable');

	await page.mouse.move(
		porchBounds.x + porchBounds.width / 2,
		porchBounds.y + porchBounds.height / 2
	);
	await page.mouse.down();
	await page.mouse.move(
		porchBounds.x + porchBounds.width / 2,
		porchBounds.y + porchBounds.height / 2 + canvasBounds.height / 12
	);
	await page.mouse.up();
	await expect(porchTile).toHaveAttribute('data-layout-row', '2');

	await page.getByRole('button', { name: 'Undo' }).click();
	await expect(porchTile).toHaveAttribute('data-layout-row', '1');
	await porchButton.focus();
	await page.keyboard.press('ArrowDown');
	await expect(porchTile).toHaveAttribute('data-layout-row', '2');
	await page.getByRole('button', { name: 'Undo' }).click();
	await page.getByRole('button', { name: 'Select Front Door layout tile' }).click();
	await page.getByRole('button', { name: 'Resize Front Door layout tile' }).focus();
	await page.keyboard.press('ArrowLeft');
	await expect(page.locator('[data-peek-layout-item="front-door"]')).toHaveAttribute(
		'data-layout-column-span',
		'7'
	);
	await page.getByRole('button', { name: 'Undo' }).click();

	await page.getByRole('button', { name: 'Add Back Yard to layout' }).click();
	await expect(page.locator('[data-peek-layout-item="back-yard"]')).toHaveAttribute(
		'data-layout-row',
		'5'
	);
	await expect(page.getByRole('button', { name: 'Select Back Yard layout tile' })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await porchButton.click();
	await page.getByRole('button', { name: 'Porch can be promoted' }).click();
	await expect(page.getByRole('button', { name: 'Porch is pinned' })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await page.getByRole('switch', { name: 'Activity focus' }).click();
	await expect(page.getByRole('switch', { name: 'Activity focus' })).toHaveAttribute(
		'aria-checked',
		'false'
	);
	await page.getByRole('button', { name: '2x2' }).click();
	await expect(page.locator('[data-peek-layout-item="front-door"]')).toHaveAttribute(
		'data-layout-column-span',
		'6'
	);

	await page.getByRole('button', { name: 'Discard' }).click();
	await expect(page.locator('[data-peek-layout-editor]')).toHaveCount(0);
	await page
		.getByRole('region', { name: 'Dashboards' })
		.getByRole('button', { name: 'Edit grid' })
		.click();
	await expect(page.locator('[data-peek-layout-item]')).toHaveCount(3);
	await page.getByRole('button', { name: '2x2' }).click();
	await page.getByRole('button', { name: 'Done', exact: true }).click();
	await expect.poll(() => controls.peekLayoutUpdates.length).toBe(1);
	expect(browserErrors).toEqual([]);
});

test('keeps dashboard mutations out of the Dashboard route', async ({ page }) => {
	await mockMixedHealth(page);
	await page.goto('/?mode=layout-editor');

	await expect(page.locator('[data-peek-layout-editor]')).toHaveCount(0);
	await expect(page.locator('[data-peek-dashboard-switcher]')).toBeVisible();
	await expect(page.getByRole('button', { name: 'New dashboard' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Delete dashboard' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Edit grid' })).toHaveCount(0);
});

test('persists an accessible dashboard selection for a User without mutation controls', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	const controls = await mockControlPeer(page, {
		cameras: mixedCameras,
		health: mixedHealth,
		accessRole: 'user',
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		peekLayoutRegistry: {
			schema_version: 1,
			active_layout_id: 'front-of-house',
			layouts: [
				{
					id: 'front-of-house',
					name: 'Front of house',
					scope: 'shared',
					owner_id: 'server',
					activity_focus: true,
					tiles: [
						{
							camera_id: 'front-door',
							column: 1,
							row: 1,
							column_span: 8,
							row_span: 12,
							pinned: true
						},
						{
							camera_id: 'porch',
							column: 9,
							row: 1,
							column_span: 4,
							row_span: 6,
							pinned: false
						},
						{
							camera_id: 'alley',
							column: 9,
							row: 7,
							column_span: 4,
							row_span: 6,
							pinned: false
						}
					]
				},
				{
					id: 'everything',
					name: 'Everything',
					scope: 'shared',
					owner_id: 'server',
					activity_focus: false,
					tiles: mixedCameras.map((camera, index) => ({
						camera_id: camera.id,
						column: (index % 2) * 6 + 1,
						row: Math.floor(index / 2) * 6 + 1,
						column_span: 6,
						row_span: 6,
						pinned: false
					}))
				}
			]
		}
	});

	await page.goto('/');
	let dashboardTrigger = page.getByRole('button', {
		name: 'Choose dashboard, Front of house'
	});
	await dashboardTrigger.click();
	await page.getByRole('menuitemradio', { name: 'Everything' }).click();
	await expect(page.getByRole('button', { name: 'Choose dashboard, Everything' })).toBeVisible();
	await expect(page.locator('[data-peek-wall-content]')).toHaveAttribute(
		'data-peek-layout-id',
		'everything'
	);
	await expect.poll(() => controls.peekLayoutUpdates.length).toBe(1);

	await page.reload();
	dashboardTrigger = page.getByRole('button', { name: 'Choose dashboard, Everything' });
	await expect(dashboardTrigger).toBeVisible();
	await expect(page.getByRole('button', { name: 'Edit grid' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Manage access' })).toHaveCount(0);
	await expect.poll(() => controls.peekLayoutUpdates.length).toBe(1);
	expect(browserErrors).toEqual([]);
});

test('creates, renames, duplicates, deletes, exports, and imports dashboards in Settings', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	const initialRegistry = editableDashboardRegistry();
	initialRegistry.active_layout_id = 'default';
	initialRegistry.layouts = [initialRegistry.layouts[0]!];
	const controls = await mockControlPeer(page, {
		cameras: mixedCameras,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		runtimeConfiguration: settingsRuntimeConfiguration,
		peekLayoutRegistry: initialRegistry
	});

	await page.goto('/settings#dashboards');
	const dashboards = page.getByRole('region', { name: 'Dashboards' });
	const layoutSelect = dashboards.getByLabel('Dashboard to manage');
	await dashboards.getByRole('button', { name: 'New dashboard' }).click();
	await page.getByRole('textbox', { name: 'Dashboard name' }).fill('Patio');
	await page.getByRole('checkbox', { name: 'Everyone with KeepPeek access' }).check();
	await page.getByRole('dialog').getByRole('button', { name: 'Save' }).click();
	await expect(layoutSelect.locator('option:checked')).toHaveText('Patio');
	await expect.poll(() => controls.peekLayoutUpdates.length).toBe(1);
	const createdLayouts = controls.peekLayoutUpdates[0]?.layouts as Array<Record<string, unknown>>;
	expect(createdLayouts.find((layout) => layout.name === 'Patio')).toMatchObject({
		scope: 'shared',
		owner_id: 'server',
		audience: { everyone: true, credential_ids: [] }
	});

	await dashboards.getByRole('button', { name: 'Rename dashboard' }).click();
	await page.getByRole('textbox', { name: 'Dashboard name' }).fill('Patio cameras');
	await page.getByRole('dialog').getByRole('button', { name: 'Save' }).click();
	await expect(layoutSelect.locator('option:checked')).toHaveText('Patio cameras');

	await dashboards.getByRole('button', { name: 'Duplicate dashboard' }).click();
	await expect(layoutSelect.locator('option:checked')).toHaveText('Patio cameras copy');
	await dashboards.getByRole('button', { name: 'Delete dashboard' }).click();
	await dashboards.getByRole('button', { name: 'Confirm delete dashboard' }).click();
	await expect(layoutSelect.locator('option:checked')).toHaveText('All cameras');
	await expect(layoutSelect.locator('option')).toHaveText(['All cameras', 'Patio cameras']);

	await dashboards.getByLabel('Export dashboards').click();
	const downloadPromise = page.waitForEvent('download');
	await page.getByRole('button', { name: 'All dashboards' }).click();
	const download = await downloadPromise;
	expect(download.suggestedFilename()).toBe('keeppeek-layouts.json');
	const downloadPath = await download.path();
	expect(downloadPath).not.toBeNull();
	const exported = await readFile(downloadPath!, 'utf8');
	expect(exported).toContain('"schema_version": 1');
	expect(exported).toContain('"Patio cameras"');
	expect(exported).not.toMatch(/access[_-]?key|password/i);

	await page.getByLabel('Choose dashboard import file').setInputFiles({
		name: 'imported-layout.json',
		mimeType: 'application/json',
		buffer: Buffer.from(
			JSON.stringify({
				schema_version: 1,
				active_layout_id: 'imported',
				layouts: [
					{
						id: 'imported',
						name: 'Imported',
						scope: 'private',
						owner_id: 'another-installation',
						activity_focus: false,
						tiles: [
							{
								camera_id: 'legacy-side',
								column: 1,
								row: 1,
								column_span: 12,
								row_span: 12,
								pinned: true
							}
						]
					}
				]
			})
		)
	});
	const importDialog = page.getByRole('dialog', { name: 'Import dashboards' });
	await expect(importDialog).toBeVisible();
	await importDialog.getByLabel('legacy-side').selectOption('porch');
	await importDialog.getByRole('button', { name: 'Import', exact: true }).click();
	await expect(importDialog).toHaveCount(0);
	await expect(layoutSelect.locator('option:checked')).toHaveText('Imported');
	await expect.poll(() => controls.peekLayoutUpdates.length).toBe(5);
	const importedRegistry = controls.peekLayoutUpdates.at(-1)!;
	const importedLayouts = importedRegistry.layouts as Array<Record<string, unknown>>;
	const importedLayout = importedLayouts.find((layout) => layout.id === 'imported');
	expect(importedLayout).toMatchObject({
		scope: 'shared',
		owner_id: 'server',
		audience: { everyone: false, credential_ids: [] },
		activity_focus: false
	});
	expect(importedLayout?.tiles).toEqual([
		expect.objectContaining({ camera_id: 'porch', pinned: true })
	]);

	await page.reload();
	await expect(layoutSelect.locator('option:checked')).toHaveText('Imported');
	expect(browserErrors).toEqual([]);
});

test('preserves the Settings grid draft when a stale dashboard save conflicts', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const controls = await mockControlPeer(page, {
		cameras: mixedCameras,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		runtimeConfiguration: settingsRuntimeConfiguration,
		peekLayoutConflictOnSave: true,
		peekLayoutRegistry: editableDashboardRegistry()
	});

	await page.goto('/settings#dashboards');
	await page
		.getByRole('region', { name: 'Dashboards' })
		.getByRole('button', { name: 'Edit grid' })
		.click();
	await page.getByRole('button', { name: '2x2', exact: true }).click();
	await page.getByRole('button', { name: 'Done', exact: true }).click();

	await expect(page.getByRole('alert')).toContainText(
		'Peek layout registry revision conflict (current revision 2)'
	);
	await expect(page.locator('[data-peek-layout-item="front-door"]')).toHaveAttribute(
		'data-layout-column-span',
		'6'
	);
	await expect(page.getByRole('button', { name: 'Done', exact: true })).toBeEnabled();
	expect(controls.peekLayoutUpdates).toEqual([]);
});

test('preserves the Settings grid draft when dashboard persistence is unavailable', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockControlPeer(page, {
		cameras: mixedCameras,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		runtimeConfiguration: settingsRuntimeConfiguration,
		peekLayoutRegistry: editableDashboardRegistry(),
		peekLayoutSaveError: 'Dashboard persistence is unavailable'
	});

	await page.goto('/settings#dashboards');
	await page
		.getByRole('region', { name: 'Dashboards' })
		.getByRole('button', { name: 'Edit grid' })
		.click();
	await page.getByRole('button', { name: '2x2', exact: true }).click();
	await page.getByRole('button', { name: 'Done', exact: true }).click();

	await expect(page.getByRole('alert')).toContainText('Dashboard persistence is unavailable');
	await expect(page.locator('[data-peek-layout-item="front-door"]')).toHaveAttribute(
		'data-layout-column-span',
		'6'
	);
	await expect(page.getByRole('button', { name: 'Done', exact: true })).toBeEnabled();
});

test('shows a retryable Settings error when the dashboard registry cannot be loaded', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockControlPeer(page, {
		cameras: mixedCameras,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		runtimeConfiguration: settingsRuntimeConfiguration
	});

	await page.goto('/settings#dashboards');

	const dashboards = page.getByRole('region', { name: 'Dashboards' });
	await expect(dashboards.getByRole('status')).toContainText('Peek layout state is not configured');
	await expect(dashboards.getByRole('button', { name: 'Retry' })).toBeEnabled();
	await expect(page.locator('[data-peek-layout-editor]')).toHaveCount(0);
});

test('retains a removed camera as a clear placeholder until the user removes it', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const registry = editableDashboardRegistry();
	registry.layouts[1]!.tiles = [
		{
			camera_id: 'front-door',
			column: 1,
			row: 1,
			column_span: 6,
			row_span: 12,
			pinned: true
		},
		{
			camera_id: 'removed-camera',
			column: 7,
			row: 1,
			column_span: 6,
			row_span: 12,
			pinned: false
		}
	];
	const controls = await mockControlPeer(page, {
		cameras: mixedCameras,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		runtimeConfiguration: settingsRuntimeConfiguration,
		peekLayoutRegistry: registry
	});

	await page.goto('/');
	await expect(page.locator('[data-peek-missing-camera="removed-camera"]')).toContainText(
		'Camera unavailable'
	);
	await page.goto('/settings#dashboards');
	await page
		.getByRole('region', { name: 'Dashboards' })
		.getByRole('button', { name: 'Edit grid' })
		.click();
	await page
		.getByRole('button', { name: 'Select unavailable camera removed-camera layout tile' })
		.click();
	await page.getByRole('button', { name: 'Remove from layout' }).click();
	await expect(page.locator('[data-peek-layout-item="removed-camera"]')).toHaveCount(0);
	await page.getByRole('button', { name: 'Done', exact: true }).click();

	await expect(page).toHaveURL(/\/settings#dashboards$/);
	await expect.poll(() => controls.peekLayoutUpdates.length).toBe(1);
	const saved = JSON.stringify(controls.peekLayoutUpdates[0]);
	expect(saved).not.toContain('removed-camera');
});

test('renders a saved Dashboard without mobile management controls or overflow', async ({
	page
}) => {
	await page.setViewportSize({ width: 375, height: 667 });
	await mockControlPeer(page, {
		cameras: mixedCameras,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		peekLayoutRegistry: {
			schema_version: 1,
			active_layout_id: 'mobile',
			layouts: [
				{
					id: 'mobile',
					name: 'Mobile wall',
					scope: 'shared',
					owner_id: 'server',
					activity_focus: false,
					tiles: mixedCameras.map((camera, index) => ({
						camera_id: camera.id,
						column: (index % 2) * 6 + 1,
						row: Math.floor(index / 2) * 6 + 1,
						column_span: 6,
						row_span: 6,
						pinned: false
					}))
				}
			]
		}
	});

	await page.goto('/');
	await expect(page.locator('[data-peek-dashboard-switcher]')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Edit grid' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Manage access' })).toHaveCount(0);
	await expect(page.locator('[data-peek-wall-content] [data-peek-camera]')).toHaveCount(4);
	const hasHorizontalOverflow = await page.evaluate(
		() => document.documentElement.scrollWidth > document.documentElement.clientWidth
	);
	expect(hasHorizontalOverflow).toBe(false);
	const tileBounds = await page
		.locator('[data-peek-wall-content] [data-peek-camera]')
		.evaluateAll((tiles) =>
			tiles.map((tile) => {
				const bounds = tile.getBoundingClientRect();
				return { left: bounds.left, right: bounds.right, top: bounds.top, bottom: bounds.bottom };
			})
		);
	for (const [index, bounds] of tileBounds.entries()) {
		expect(bounds.left).toBeGreaterThanOrEqual(0);
		expect(bounds.right).toBeLessThanOrEqual(375);
		for (const other of tileBounds.slice(0, index)) {
			const overlaps =
				bounds.left < other.right &&
				bounds.right > other.left &&
				bounds.top < other.bottom &&
				bounds.bottom > other.top;
			expect(overlaps).toBe(false);
		}
	}
});

test('floats the dashboard selector over a full-shell nine-camera wall', async ({ page }) => {
	await page.setViewportSize({ width: 1188, height: 624 });
	const cameras = Array.from({ length: 9 }, (_, index) => ({
		...mixedCameras[index % mixedCameras.length],
		id: `camera-${index + 1}`,
		ip: `192.0.2.${index + 1}`,
		name: `Camera ${index + 1}`
	}));
	await mockControlPeer(page, {
		cameras,
		health: mixedHealth,
		capabilityIds: ['keeppeek.peek-layouts.v1'],
		peekLayoutRegistry: {
			schema_version: 1,
			active_layout_id: 'nine-camera-wall',
			layouts: [
				{
					id: 'nine-camera-wall',
					name: 'Nine camera wall',
					scope: 'shared',
					owner_id: 'server',
					activity_focus: false,
					tiles: cameras.map((camera, index) => ({
						camera_id: camera.id,
						column: (index % 3) * 4 + 1,
						row: Math.floor(index / 3) * 4 + 1,
						column_span: 4,
						row_span: 4,
						pinned: false
					}))
				}
			]
		}
	});

	await page.goto('/');
	await expect(page.locator('[data-peek-wall-content] [data-peek-camera]')).toHaveCount(9);
	const dashboardSwitcher = page.locator('[data-peek-dashboard-switcher]');
	await expect(dashboardSwitcher).toBeVisible();
	await expect(dashboardSwitcher).not.toContainText('Peek');
	await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toHaveCount(1);
	await expect(page.getByRole('heading', { name: 'Peek', exact: true })).toHaveCount(0);
	const dashboardTrigger = page.getByRole('button', {
		name: 'Choose dashboard, Nine camera wall'
	});
	await expect(dashboardTrigger).toBeVisible();
	await expect(page.locator('[data-peek-dashboard-switcher] select')).toHaveCount(0);
	await dashboardTrigger.click();
	const dashboardMenu = page.getByRole('menu', { name: 'Choose dashboard' });
	await expect(dashboardMenu).toBeVisible();
	await expect(
		dashboardMenu.getByRole('menuitemradio', { name: 'Nine camera wall' })
	).toHaveAttribute('aria-checked', 'true');
	await page.keyboard.press('Escape');
	await expect(page.getByRole('button', { name: 'Edit grid' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'New layout' })).toHaveCount(0);

	const geometry = () =>
		page.evaluate(() => {
			const main = document.querySelector<HTMLElement>('[data-shell-main]');
			const view = document.querySelector<HTMLElement>('[data-peek-view]');
			const content = document.querySelector<HTMLElement>('[data-peek-view-content]');
			const switcher = document.querySelector<HTMLElement>('[data-peek-dashboard-switcher]');
			const status = document.querySelector<HTMLElement>('[data-shell-status-indicators]');
			const theme = document.querySelector<HTMLElement>('button[aria-label^="Switch to "]');
			const frame = document.querySelector<HTMLElement>('[data-peek-wall]');
			const wall = document.querySelector<HTMLElement>('[data-peek-wall-content]');
			if (!main || !view || !content || !switcher || !status || !theme || !frame || !wall) {
				throw new Error('Peek wall geometry is unavailable');
			}
			const mainBounds = main.getBoundingClientRect();
			const viewBounds = view.getBoundingClientRect();
			const contentBounds = content.getBoundingClientRect();
			const switcherBounds = switcher.getBoundingClientRect();
			const statusBounds = status.getBoundingClientRect();
			const themeBounds = theme.getBoundingClientRect();
			const frameBounds = frame.getBoundingClientRect();
			const wallBounds = wall.getBoundingClientRect();
			const tileBottom = Math.max(
				...Array.from(
					wall.querySelectorAll<HTMLElement>('[data-peek-camera]'),
					(tile) => tile.getBoundingClientRect().bottom
				)
			);
			return {
				mainClientHeight: main.clientHeight,
				mainScrollHeight: main.scrollHeight,
				main: mainBounds.toJSON(),
				view: viewBounds.toJSON(),
				content: contentBounds.toJSON(),
				switcher: switcherBounds.toJSON(),
				status: statusBounds.toJSON(),
				theme: themeBounds.toJSON(),
				frame: frameBounds.toJSON(),
				wall: wallBounds.toJSON(),
				switcherCenterOffset: Math.abs(
					switcherBounds.left + switcherBounds.width / 2 - (mainBounds.left + mainBounds.width / 2)
				),
				switcherOwnsCenter:
					document.elementFromPoint(
						switcherBounds.left + switcherBounds.width / 2,
						switcherBounds.top + switcherBounds.height / 2
					) === switcher ||
					switcher.contains(
						document.elementFromPoint(
							switcherBounds.left + switcherBounds.width / 2,
							switcherBounds.top + switcherBounds.height / 2
						)
					),
				tileBottom
			};
		});

	for (const viewport of [
		{ width: 1188, height: 624 },
		{ width: 1024, height: 768 }
	]) {
		await page.setViewportSize(viewport);
		const bounds = await geometry();
		expect(bounds.mainScrollHeight).toBeLessThanOrEqual(bounds.mainClientHeight + 1);
		expect(bounds.view.left).toBeCloseTo(bounds.main.left, 0);
		expect(bounds.view.top).toBeCloseTo(bounds.main.top, 0);
		expect(bounds.view.right).toBeCloseTo(bounds.main.right, 0);
		expect(bounds.view.bottom).toBeCloseTo(bounds.main.bottom, 0);
		expect(bounds.content.left).toBeCloseTo(bounds.main.left, 0);
		expect(bounds.content.top).toBeCloseTo(bounds.main.top, 0);
		expect(bounds.content.right).toBeCloseTo(bounds.main.right, 0);
		expect(bounds.content.bottom).toBeCloseTo(bounds.main.bottom, 0);
		expect(bounds.frame.left).toBeCloseTo(bounds.main.left, 0);
		expect(bounds.frame.top).toBeCloseTo(bounds.main.top, 0);
		expect(bounds.frame.right).toBeCloseTo(bounds.main.right, 0);
		expect(bounds.frame.bottom).toBeCloseTo(bounds.main.bottom, 0);
		expect(bounds.wall.width).toBeCloseTo(
			Math.min(bounds.main.width, (bounds.main.height * 16) / 9),
			0
		);
		expect(bounds.wall.height).toBeCloseTo(
			Math.min(bounds.main.height, (bounds.main.width * 9) / 16),
			0
		);
		expect(bounds.switcherCenterOffset).toBeLessThanOrEqual(1);
		expect(bounds.switcher.top).toBeGreaterThanOrEqual(bounds.main.top + 8);
		expect(bounds.switcherOwnsCenter).toBe(true);
		expect(bounds.status.left).toBeLessThan(bounds.content.left + 1);
		expect(bounds.status.bottom).toBeLessThanOrEqual(bounds.theme.top);
		expect(bounds.tileBottom).toBeLessThanOrEqual(bounds.main.bottom + 1);
	}
});
