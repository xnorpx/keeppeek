import { expect, test } from '@playwright/test';

test('renders the KeepPeek dashboard without configured cameras', async ({ page }) => {
	await page.route('**/health', async (route) => {
		await route.fulfill({ json: { status: 'ok', cameras: [] } });
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({ json: [] });
	});

	await page.goto('/');

	await expect(page).toHaveTitle('Peek - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Peek', exact: true })).toBeVisible();
	await expect(page.getByText('System online', { exact: true })).toBeVisible();
	await expect(page.getByText('0 cameras', { exact: true })).toBeVisible();
	await expect(page.getByText('No cameras configured.')).toBeVisible();
	await page.getByRole('button', { name: 'Create saved view' }).click();
	await expect(page.getByText('No cameras are available.')).toBeVisible();
});

test('saves and restores a custom Peek view with selected streams', async ({ page }) => {
	await page.addInitScript(() => {
		if (sessionStorage.getItem('peek-layout-test-initialized')) return;
		localStorage.clear();
		sessionStorage.setItem('peek-layout-test-initialized', 'true');
	});
	await page.route('**/health', async (route) => {
		await route.fulfill({ json: { status: 'ok', cameras: [] } });
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({
			json: [
				{
					id: 'front-door',
					ip: '192.0.2.1',
					name: 'Front Door',
					manufacturer: 'Reolink',
					model: 'RLC-820A',
					firmware_version: null,
					is_reolink: true,
					profiles: [
						{
							name: 'Main',
							stream: 'main',
							encoding: 'h264',
							resolution: '3840x2160',
							framerate: 20
						},
						{
							name: 'Sub',
							stream: 'sub',
							encoding: 'h264',
							resolution: '640x360',
							framerate: 15
						}
					]
				},
				{
					id: 'garage',
					ip: '192.0.2.2',
					name: 'Garage',
					manufacturer: 'Reolink',
					model: 'RLC-810A',
					firmware_version: null,
					is_reolink: true,
					profiles: [
						{
							name: 'Main',
							stream: 'main',
							encoding: 'h264',
							resolution: '3840x2160',
							framerate: 20
						},
						{
							name: 'Sub',
							stream: 'sub',
							encoding: 'h264',
							resolution: '640x360',
							framerate: 15
						}
					]
				},
				{
					id: 'backyard',
					ip: '192.0.2.3',
					name: 'Backyard',
					manufacturer: 'Reolink',
					model: 'RLC-510A',
					firmware_version: null,
					is_reolink: true,
					profiles: [
						{
							name: 'Main',
							stream: 'main',
							encoding: 'h264',
							resolution: '2560x1440',
							framerate: 20
						}
					]
				}
			]
		});
	});
	await page.route('**/api/live/browser/offer', async (route) => {
		await route.fulfill({
			status: 503,
			json: { error: 'Live playback is not part of this layout test.' }
		});
	});

	await page.goto('/');
	await expect(page.getByText('3 cameras', { exact: true })).toBeVisible();

	await page.getByRole('button', { name: 'Create saved view' }).click();
	await expect(page.getByRole('form', { name: 'View editor' })).toBeVisible();
	await expect(page.getByRole('region', { name: 'Dynamic live view' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Matrix' })).toHaveCount(0);
	await expect(page.locator('#peek-view-rows, #peek-view-columns')).toHaveCount(0);
	await page.getByLabel('View name').fill('Entryway');
	const customSlots = page.locator('[aria-label="Custom view slots"] [role="listitem"]');
	await expect(customSlots.nth(0)).toContainText('Front Door');
	await expect(customSlots.nth(1)).toContainText('Garage');
	await expect(customSlots.nth(2)).toContainText('Backyard');
	await expect(page.locator('[data-camera-source]')).toHaveCount(0);
	for (const template of [
		'1 Camera',
		'2 Grid',
		'3 Grid',
		'5 Focus',
		'6 Grid',
		'6 Focus Left',
		'6 Focus Right',
		'7 Focus Left',
		'7 Focus Right',
		'8 Grid',
		'8 Mosaic',
		'9 Focus Left',
		'9 Focus Right',
		'9 Focus Bottom',
		'10 Focus Left',
		'10 Focus Right',
		'10 Grid'
	]) {
		await expect(page.getByRole('button', { name: `Choose ${template} layout` })).toBeVisible();
	}
	await expect(page.getByRole('button', { name: 'Choose 4 Grid layout' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Choose 9 Grid layout' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Choose 10 Focus Left layout' })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await page.getByRole('button', { name: 'Choose 6 Grid layout' }).click();
	await expect(customSlots).toHaveCount(6);
	await expect(customSlots.first()).toHaveCSS('grid-column-end', 'span 1');
	await expect(customSlots.nth(0)).toContainText('Front Door');
	await expect(customSlots.nth(1)).toContainText('Garage');
	await expect(customSlots.nth(2)).toContainText('Backyard');
	await page.getByRole('button', { name: 'Choose 6 Focus Left layout' }).click();
	await expect(customSlots.first()).toHaveCSS('grid-column-end', 'span 2');
	await page.getByRole('button', { name: 'Choose 6 Focus Right layout' }).click();
	await expect(customSlots.first()).toHaveCSS('grid-column-end', 'span 1');
	await expect(customSlots.nth(4)).toHaveCSS('grid-column-end', 'span 2');
	await page.getByRole('button', { name: 'Choose 9 Focus Right layout' }).click();
	await expect(customSlots.first()).toHaveCSS('grid-column-start', '3');
	await expect(customSlots.first()).toHaveCSS('grid-column-end', 'span 2');
	await page.getByRole('button', { name: 'Choose 9 Focus Bottom layout' }).click();
	await expect(customSlots.first()).toHaveCSS('grid-row-start', '2');
	await expect(customSlots.first()).toHaveCSS('grid-row-end', 'span 2');
	await page.getByRole('button', { name: 'Choose 10 Grid layout' }).click();
	await expect(customSlots).toHaveCount(10);
	await expect(customSlots.first()).toHaveCSS('grid-column-end', 'span 1');
	await page.getByRole('button', { name: 'Choose 10 Focus Left layout' }).click();
	await expect(customSlots.nth(0)).toHaveCSS('grid-column-start', '1');
	await expect(customSlots.nth(0)).toHaveCSS('grid-column-end', 'span 2');
	await expect(customSlots.nth(1)).toHaveCSS('grid-row-start', '3');
	await expect(customSlots.nth(1)).toHaveCSS('grid-row-end', 'span 2');
	await expect(page.locator('[data-grid-move-handle]')).toHaveCount(0);
	await expect(page.locator('[data-grid-resize-handle]')).toHaveCount(0);
	await expect(page.locator('#peek-grid-column')).toHaveCount(0);
	const firstCustomSlot = customSlots.first();
	const cameraStrip = page.locator('[data-camera-strip]');
	await expect(firstCustomSlot).toContainText('Front Door');
	await expect(customSlots.nth(1)).toContainText('Garage');
	await expect(customSlots.nth(2)).toContainText('Backyard');
	await expect(cameraStrip.locator('[data-camera-source]')).toHaveCount(0);
	await expect(cameraStrip.locator('[data-camera-strip-preview] video')).toHaveCount(0);
	await expect(cameraStrip.getByRole('button', { name: 'WebRTC stream diagnostics' })).toHaveCount(
		0
	);
	await firstCustomSlot.getByRole('button', { name: 'Use main stream for Front Door' }).click();
	await expect(
		firstCustomSlot.getByRole('button', { name: 'Use main stream for Front Door' })
	).toHaveAttribute('aria-pressed', 'true');
	await page.getByRole('button', { name: 'Dynamic' }).click();
	await expect(customSlots).toHaveCount(0);
	await expect(page.getByRole('region', { name: 'Dynamic live view preview' })).toBeVisible();
	await expect(page.locator('[data-dynamic-editor-tile]')).toHaveCount(3);
	await expect(page.locator('[data-dynamic-editor-tile] video')).toHaveCount(3);
	await page.getByRole('button', { name: 'Preset' }).click();
	await expect(customSlots).toHaveCount(10);
	await expect(firstCustomSlot).toContainText('Front Door');
	await expect(
		firstCustomSlot.getByRole('button', { name: 'Use main stream for Front Door' })
	).toHaveAttribute('aria-pressed', 'true');
	await customSlots.nth(2).getByRole('button', { name: 'Clear slot 3' }).click();
	await expect(cameraStrip.locator('[data-camera-source]')).toHaveCount(1);
	await expect(cameraStrip.locator('[data-camera-strip-preview="backyard"]')).toContainText('main');
	const backyardSource = cameraStrip.locator('[data-camera-source="backyard"]');
	await expect(backyardSource).toHaveCSS('cursor', 'grab');
	const dragData = await page.evaluateHandle(() => new DataTransfer());
	await backyardSource.dispatchEvent('dragstart', {
		dataTransfer: dragData,
		clientX: 120,
		clientY: 160
	});
	const dragPreview = page.locator('[data-camera-drag-preview="backyard"]');
	await expect(dragPreview).toBeVisible();
	await expect(dragPreview.locator('video')).toHaveCount(1);
	await backyardSource.dispatchEvent('drag', {
		dataTransfer: dragData,
		clientX: 240,
		clientY: 300
	});
	await expect(dragPreview).toHaveAttribute('style', /left: 240px; top: 300px/);
	await backyardSource.dispatchEvent('dragend', { dataTransfer: dragData });
	await expect(dragPreview).toHaveCount(0);
	await backyardSource.dragTo(customSlots.nth(2));
	await expect(customSlots.nth(2)).toContainText('Backyard');
	await expect(cameraStrip.locator('[data-camera-source]')).toHaveCount(0);
	const garageDragHandle = customSlots.nth(1).locator('[data-grid-tile-drag-handle="1"]');
	await customSlots.nth(1).scrollIntoViewIfNeeded();
	await expect(garageDragHandle).toHaveCSS('cursor', 'grab');
	const presetGarageHandleBox = await garageDragHandle.boundingBox();
	const presetTargetSlotBox = await customSlots.nth(3).boundingBox();
	if (!presetGarageHandleBox || !presetTargetSlotBox) {
		throw new Error('Preset tile controls were not visible');
	}
	await page.mouse.move(
		presetGarageHandleBox.x + presetGarageHandleBox.width / 2,
		presetGarageHandleBox.y + presetGarageHandleBox.height / 2
	);
	await page.mouse.down();
	await page.mouse.move(
		presetTargetSlotBox.x + presetTargetSlotBox.width / 2,
		presetTargetSlotBox.y + presetTargetSlotBox.height / 2,
		{ steps: 5 }
	);
	await page.mouse.up();
	await expect(customSlots.nth(1)).toContainText('Drop camera');
	await expect(customSlots.nth(3)).toContainText('Garage');
	await page.getByRole('button', { name: 'Save view' }).click();

	await expect(page.locator('#peek-layout-select option:checked')).toHaveText('Entryway');
	await expect(page.locator('[data-camera-id]')).toHaveCount(3);
	await expect(page.locator('[data-camera-id="front-door"]')).toHaveAttribute(
		'data-requested-quality',
		'auto'
	);
	await expect(page.locator('[data-camera-id="garage"]')).toHaveAttribute(
		'data-requested-quality',
		'low'
	);
	await expect(page.locator('[data-camera-id="front-door"]').locator('xpath=..')).toHaveCSS(
		'grid-column-start',
		'1'
	);
	await expect(page.locator('[data-camera-id="garage"]').locator('xpath=..')).toHaveCSS(
		'grid-column-start',
		'4'
	);
	await expect(page.locator('[data-camera-id="garage"]').locator('xpath=..')).toHaveCSS(
		'grid-column-end',
		'span 1'
	);

	await page.getByRole('button', { name: 'Edit selected view' }).click();
	await page.getByRole('button', { name: 'Dynamic' }).click();
	const dynamicTiles = page.locator('[data-dynamic-editor-tile]');
	await expect(dynamicTiles).toHaveCount(3);
	expect(
		await dynamicTiles.evaluateAll((tiles) => tiles.map((tile) => tile.dataset.dynamicEditorTile))
	).toEqual(['front-door', 'garage', 'backyard']);
	const garageDynamicDragHandle = dynamicTiles
		.nth(1)
		.locator('[data-dynamic-tile-drag-handle="1"]');
	await expect(garageDynamicDragHandle).toHaveCSS('cursor', 'grab');
	const garageHandleBox = await garageDynamicDragHandle.boundingBox();
	const frontDoorTileBox = await dynamicTiles.nth(0).boundingBox();
	if (!garageHandleBox || !frontDoorTileBox) {
		throw new Error('Dynamic tile controls were not visible');
	}
	await page.mouse.move(
		garageHandleBox.x + garageHandleBox.width / 2,
		garageHandleBox.y + garageHandleBox.height / 2
	);
	await page.mouse.down();
	await page.mouse.move(
		frontDoorTileBox.x + frontDoorTileBox.width / 2,
		frontDoorTileBox.y + frontDoorTileBox.height / 2,
		{ steps: 5 }
	);
	await page.mouse.up();
	expect(
		await dynamicTiles.evaluateAll((tiles) => tiles.map((tile) => tile.dataset.dynamicEditorTile))
	).toEqual(['garage', 'front-door', 'backyard']);
	await page.getByRole('button', { name: 'Save view' }).click();
	expect(
		await page
			.locator('[data-camera-id]')
			.evaluateAll((tiles) => tiles.map((tile) => tile.dataset.cameraId))
	).toEqual(['garage', 'front-door', 'backyard']);

	await page.reload();

	await expect(page.locator('#peek-layout-select option:checked')).toHaveText('Entryway');
	expect(
		await page
			.locator('[data-camera-id]')
			.evaluateAll((tiles) => tiles.map((tile) => tile.dataset.cameraId))
	).toEqual(['garage', 'front-door', 'backyard']);
});
