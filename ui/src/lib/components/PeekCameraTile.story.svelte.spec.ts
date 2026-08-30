import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import LightThemePeekStory from '../../../visual-harness/stories/LightThemePeekStory.svelte';

describe('Board 34 light-theme Dashboard story', () => {
	it('renders the exact Paper frame with production tile states and fonts', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(LightThemePeekStory);

		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="peek.desktop.light-theme"]'
		);
		expect(frame).not.toBeNull();
		const frameBounds = frame!.getBoundingClientRect();
		expect([Math.round(frameBounds.width), Math.round(frameBounds.height)]).toEqual([1440, 362]);

		const tiles = [...container.querySelectorAll<HTMLElement>('[data-peek-camera]')];
		expect(tiles.map((tile) => tile.dataset.peekCameraState)).toEqual([
			'healthy',
			'degraded',
			'offline'
		]);
		expect(
			tiles.map((tile) => {
				const bounds = tile.getBoundingClientRect();
				return [
					Math.round(bounds.x - frameBounds.x),
					Math.round(bounds.y - frameBounds.y),
					Math.round(bounds.width),
					Math.round(bounds.height)
				];
			})
		).toEqual([
			[73, 9, 446, 344],
			[527, 9, 448, 344],
			[983, 9, 448, 344]
		]);
		const selector = container.querySelector<HTMLElement>('[data-peek-dashboard-switcher]');
		expect(selector).not.toBeNull();
		expect(selector!.textContent?.replace(/\s+/g, ' ').trim()).toBe('All cameras');
		expect(selector!.textContent).not.toContain('PEEK');
		const selectorBounds = selector!.getBoundingClientRect();
		const dashboardBounds = selector!.parentElement!.getBoundingClientRect();
		expect(
			Math.abs(
				selectorBounds.left +
					selectorBounds.width / 2 -
					(dashboardBounds.left + dashboardBounds.width / 2)
			)
		).toBeLessThanOrEqual(1);
		await expect
			.element(page.getByRole('link', { name: 'Dashboard' }))
			.toHaveAttribute('aria-current', 'page');
		await expect
			.element(page.getByRole('link', { name: 'Viewer' }))
			.not.toHaveAttribute('aria-current');
		expect(container.querySelector('footer')).toBeNull();

		await expect.element(page.getByText('DEGRADED', { exact: true })).toBeVisible();
		await expect.element(page.getByText('14% of frames dropped', { exact: true })).toBeVisible();
		await expect.element(page.getByText('OFFLINE', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Last report 04:23', { exact: true })).toBeVisible();
		expect(container.textContent).not.toContain('SUB · 11FPS');

		expect(getComputedStyle(tiles[0]).backgroundColor).toBe('rgb(10, 11, 12)');
		expect(getComputedStyle(tiles[2]).backgroundColor).toBe('rgb(248, 244, 236)');
		await document.fonts.ready;
		expect(
			(await document.fonts.check('600 16px Archivo')) &&
				(await document.fonts.check('400 16px "IBM Plex Mono"'))
		).toBe(true);
	});
});
