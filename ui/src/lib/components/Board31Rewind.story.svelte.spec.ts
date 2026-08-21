import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board31RewindStory from '../../../visual-harness/stories/Board31RewindStory.svelte';

async function renderState(state: 'ready' | 'scrubbing') {
	await page.viewport(1440, 900);
	const { container } = await render(Board31RewindStory, { props: { state } });
	await document.fonts.ready;
	const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
	expect(frame).not.toBeNull();
	const bounds = frame!.getBoundingClientRect();
	expect([Math.round(bounds.width), Math.round(bounds.height)]).toEqual([464, 262]);
	return { container, bounds };
}

describe('Board 31 Peek to Keep rewind stories', () => {
	it('renders the live tile with its visible drag control', async () => {
		const { container, bounds } = await renderState('ready');
		const tile = container.querySelector<HTMLElement>('[data-peek-camera="front-door"]');
		expect(tile?.dataset.peekCameraState).toBe('live');
		const control = container.querySelector<HTMLElement>('[data-peek-rewind-control]');
		const button = control?.querySelector<HTMLElement>('button');
		expect(button).not.toBeUndefined();
		const buttonBounds = button!.getBoundingClientRect();
		expect([
			Math.round(buttonBounds.x - bounds.x),
			Math.round(buttonBounds.y - bounds.y),
			Math.round(buttonBounds.width),
			Math.round(buttonBounds.height)
		]).toEqual([204, 94, 56, 56]);
		await expect.element(page.getByText('Drag down to go back', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Front Door', { exact: true })).toBeVisible();
		await expect.element(page.getByText('SUB · 25FPS', { exact: true })).toBeVisible();
	});

	it('renders the in-grid 38-second scrub state', async () => {
		const { container, bounds } = await renderState('scrubbing');
		const state = container.querySelector<HTMLElement>('[data-peek-rewind]');
		expect(state?.dataset.peekRewindSeconds).toBe('38');
		await expect.element(page.getByText('REWINDING', { exact: true })).toBeVisible();
		await expect.element(page.getByText('06:36:45', { exact: true })).toBeVisible();
		await expect.element(page.getByText('−38s', { exact: true })).toBeVisible();
		const marker = container.querySelector<HTMLElement>('[data-rewind-marker]');
		expect(marker).not.toBeNull();
		const markerBounds = marker!.getBoundingClientRect();
		expect([
			Math.round(markerBounds.x - bounds.x),
			Math.round(markerBounds.y - bounds.y),
			Math.round(markerBounds.width),
			Math.round(markerBounds.height)
		]).toEqual([250, 220, 2, 10]);
	});
});
