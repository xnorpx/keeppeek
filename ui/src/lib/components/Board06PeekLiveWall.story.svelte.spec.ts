import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board06PeekLiveWallStory from '../../../visual-harness/stories/Board06PeekLiveWallStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 6 Dashboard live wall story', () => {
	it('renders the full-height grid with a floating selector', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board06PeekLiveWallStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="peek.desktop.live-wall"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 860]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[64, 858],
			[1374, 858]
		]);
		const main = frame!.children[1];
		const selector = frame!.querySelector<HTMLElement>('[data-peek-paper-context]');
		const grid = frame!.querySelector<HTMLElement>('[data-peek-paper-grid]');
		expect(selector).not.toBeNull();
		expect(grid).not.toBeNull();
		expect(roundedSize(selector!)[1]).toBe(32);
		expect([...grid!.children].map(roundedSize)).toEqual([
			[1358, 390],
			[1358, 390],
			[1358, 38]
		]);
		const selectorBounds = selector!.getBoundingClientRect();
		const mainBounds = main.getBoundingClientRect();
		const gridBounds = grid!.getBoundingClientRect();
		expect(
			Math.abs(
				selectorBounds.left + selectorBounds.width / 2 - (mainBounds.left + mainBounds.width / 2)
			)
		).toBeLessThanOrEqual(1);
		expect(selectorBounds.top).toBeGreaterThan(gridBounds.top);
		expect(selectorBounds.bottom).toBeLessThan(gridBounds.bottom);
		const tiles = [...frame!.querySelectorAll<HTMLElement>('[data-peek-camera]')];
		expect(tiles).toHaveLength(6);
		expect(tiles.map(roundedSize)).toEqual(Array.from({ length: 6 }, () => [445, 390]));
		const frontDoorTile = frame!.querySelector<HTMLElement>('[data-peek-camera="front-door"]');
		const frontDoorLabel = frontDoorTile?.querySelector<HTMLElement>('[data-peek-camera-label]');
		const frontDoorDiagnostics = frontDoorTile?.querySelector<HTMLElement>(
			'[aria-label="Front Door camera information"]'
		);
		expect(frontDoorTile).not.toBeNull();
		expect(frontDoorLabel).not.toBeNull();
		expect(frontDoorDiagnostics).not.toBeNull();
		const tileBounds = frontDoorTile!.getBoundingClientRect();
		const labelBounds = frontDoorLabel!.getBoundingClientRect();
		const diagnosticsBounds = frontDoorDiagnostics!.getBoundingClientRect();
		expect(frontDoorLabel).toBe(frontDoorDiagnostics);
		expect(labelBounds.left).toBeGreaterThan(tileBounds.left + tileBounds.width / 2);
		expect(labelBounds).toEqual(diagnosticsBounds);
		expect(selector!.textContent?.replace(/\s+/g, ' ').trim()).toBe('All cameras');
		expect(selector!.textContent).not.toContain('PEEK');
		expect(frame!.querySelector('[data-peek-paper-fleet-runtime]')).toBeNull();
		expect(frame!.querySelector('[data-peek-paper-status]')).toBeNull();
		expect(frame!.textContent).not.toContain('last frame');
		expect(frame!.textContent).not.toContain('SUB ·');
		await page.getByRole('button', { name: 'Front Door camera information' }).click();
		await expect.element(page.getByText('Sub stream · recording', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Camera session', { exact: true })).toBeVisible();
		await expect.element(page.getByText('10m 00s', { exact: true })).toBeVisible();
		await expect.element(page.getByText('8m 00s', { exact: true })).toBeVisible();
		await expect.element(page.getByText('5m 00s', { exact: true })).toBeVisible();
		await expect.element(page.getByText('13m 00s', { exact: true })).toBeVisible();
	});

	it('reuses all four production tile states without inventing source pagination', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board06PeekLiveWallStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();
		expect(
			frame!
				.querySelector('[data-peek-camera="front-door"]')
				?.getAttribute('data-peek-camera-state')
		).toBe('healthy');
		expect(
			frame!.querySelector('[data-peek-camera="porch"]')?.getAttribute('data-peek-camera-state')
		).toBe('degraded');
		expect(
			frame!.querySelector('[data-peek-camera="alley"]')?.getAttribute('data-peek-camera-state')
		).toBe('stale');
		expect(
			frame!.querySelector('[data-peek-camera="back-yard"]')?.getAttribute('data-peek-camera-state')
		).toBe('offline');
		await expect.element(page.getByText('6 OF 6', { exact: true })).toBeVisible();
		expect(frame!.textContent).not.toContain('127');
	});
});
