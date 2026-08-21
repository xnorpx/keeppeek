import { expect, test } from '@playwright/test';
import { mockMixedHealth } from './fixtures/peek';

test('edits the 12-column Peek layout while persistence fails closed', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 840 });
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	await mockMixedHealth(page);

	await page.goto('/?mode=layout-editor');

	await expect(page).toHaveURL(/\?mode=layout-editor$/);
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

	await expect(page.getByText('Save layout', { exact: true })).toHaveCount(0);
	await expect(
		page.locator('[data-capability-gate][data-capability="keeppeek.runtime-config.v1"]')
	).toHaveCount(0);
	await expect(page.locator('[data-peek-layout-item="front-door"]')).toHaveAttribute(
		'data-layout-column-span',
		'6'
	);

	await page.getByRole('button', { name: 'Discard' }).click();
	await expect(page).toHaveURL(/\/$/);
	await expect(page.locator('[data-peek-layout-editor]')).toHaveCount(0);
	await expect(page.getByRole('heading', { name: 'Peek', exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Edit layout' }).click();
	await expect(page).toHaveURL(/\?mode=layout-editor$/);
	await expect(page.locator('[data-peek-layout-item]')).toHaveCount(3);
	expect(browserErrors).toEqual([]);
});

test('Board 8 keeps the server layout registry and deletion unavailable', async ({ page }) => {
	await mockMixedHealth(page);
	await page.goto('/?mode=layout-editor');

	await expect(page.locator('[data-peek-layout-editor]')).toBeVisible();
	await expect(page.getByRole('button', { name: 'New layout', exact: true })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Delete layout', exact: true })).toHaveCount(0);
	await expect(page.getByText('Perimeter night', { exact: true })).toHaveCount(0);
	await expect(page.getByText('Everything', { exact: true })).toHaveCount(0);
	await expect(page.locator('[data-peek-layout-item]')).toHaveCount(3);
});
