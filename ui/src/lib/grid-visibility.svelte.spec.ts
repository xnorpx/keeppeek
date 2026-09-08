import { describe, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { observeGridVisibility, type GridTileVisibility } from './grid-visibility';

describe('grid visibility in the browser', () => {
	it('updates a clipped tile when its scroll container reveals and hides it', async () => {
		await page.viewport(800, 600);
		const viewport = document.createElement('div');
		viewport.style.cssText =
			'position:fixed;top:20px;left:20px;width:200px;height:100px;overflow:auto';
		const spacer = document.createElement('div');
		spacer.style.height = '150px';
		const tile = document.createElement('div');
		tile.style.cssText = 'width:200px;height:100px';
		viewport.append(spacer, tile);
		document.body.append(viewport);
		let latest: GridTileVisibility | null = null;
		const stop = observeGridVisibility(tile, 'front', (value) => {
			latest = value;
		});
		try {
			await expect.poll(() => latest?.visibleFraction).toBe(0);
			viewport.scrollTop = 150;
			await expect.poll(() => latest?.visibleFraction).toBeCloseTo(1, 5);
			viewport.scrollTop = 0;
			await expect.poll(() => latest?.visibleFraction).toBe(0);
		} finally {
			stop();
			viewport.remove();
		}
	});

	it('notifies when a tile crosses the real viewport inside the prefetch margin', async () => {
		await page.viewport(800, 600);
		const tile = document.createElement('div');
		tile.style.cssText = 'position:fixed;left:20px;top:400px;width:100px;height:100px';
		document.body.append(tile);
		let latest: GridTileVisibility | null = null;
		const stop = observeGridVisibility(tile, 'front', (value) => {
			latest = value;
		});
		try {
			await expect.poll(() => latest?.visibleFraction).toBeCloseTo(1, 5);
			tile.style.top = '650px';
			await expect.poll(() => latest?.visibleFraction).toBe(0);
			await expect.poll(() => latest?.distanceFromViewportPx).toBe(50);
		} finally {
			stop();
			tile.remove();
		}
	});
});
