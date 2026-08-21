import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board06PeekLiveWallStory from '../../../visual-harness/stories/Board06PeekLiveWallStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 6 Peek live wall story', () => {
	it('renders the exact shell, context, grid rows, overflow, and status lanes', async () => {
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
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 774],
			[1374, 32]
		]);
		const grid = frame!.querySelector<HTMLElement>('[data-peek-paper-grid]');
		expect(grid).not.toBeNull();
		expect([...grid!.children].map(roundedSize)).toEqual([
			[1342, 340],
			[1342, 340],
			[1342, 38]
		]);
		const tiles = [...frame!.querySelectorAll<HTMLElement>('[data-peek-camera]')];
		expect(tiles).toHaveLength(6);
		expect(tiles.map(roundedSize)).toEqual(Array.from({ length: 6 }, () => [439, 340]));
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
		).toBe('live');
		expect(
			frame!.querySelector('[data-peek-camera="porch"]')?.getAttribute('data-peek-camera-state')
		).toBe('degraded');
		expect(
			frame!.querySelector('[data-peek-camera="alley"]')?.getAttribute('data-peek-camera-state')
		).toBe('reconnecting');
		expect(
			frame!.querySelector('[data-peek-camera="back-yard"]')?.getAttribute('data-peek-camera-state')
		).toBe('offline');
		await expect.element(page.getByText('6 OF 6', { exact: true })).toBeVisible();
		expect(frame!.textContent).not.toContain('127');
	});
});
