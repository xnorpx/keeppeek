import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board31HistoryStory from '../../../visual-harness/stories/Board31HistoryStory.svelte';

async function renderState(state: 'focused' | 'keep') {
	await page.viewport(1440, 900);
	const { container } = await render(Board31HistoryStory, { props: { state } });
	await document.fonts.ready;
	const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
	expect(frame).not.toBeNull();
	const bounds = frame!.getBoundingClientRect();
	expect([Math.round(bounds.width), Math.round(bounds.height)]).toEqual([464, 262]);
	return { container, bounds };
}

describe('Board 31 Focus to Keep history stories', () => {
	it('renders the focused live surface with its History action', async () => {
		const { container, bounds } = await renderState('focused');
		const focus = container.querySelector<HTMLElement>('[data-peek-focus-history]');
		expect(focus).not.toBeNull();
		const button = container.querySelector<HTMLElement>('[data-peek-history]');
		expect(button).not.toBeNull();
		const buttonBounds = button!.getBoundingClientRect();
		expect(buttonBounds.right).toBeLessThanOrEqual(bounds.x + bounds.width);
		await expect.element(page.getByText('HISTORY', { exact: true })).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Open Front Door camera' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Front Door camera information' }))
			.toBeVisible();
		expect(container.textContent).not.toContain('PEEK');
		expect(container.querySelectorAll('[data-focus-filmstrip] > span')).toHaveLength(3);
		expect(container.textContent).not.toContain('Drag down');
		expect(container.textContent).not.toContain('SUB ·');
	});

	it('renders Keep as the destination for timeline navigation', async () => {
		const { container } = await renderState('keep');
		const keep = container.querySelector<HTMLElement>('[data-history-keep]');
		expect(keep).not.toBeNull();
		await expect.element(page.getByText('From Viewer · Front Door', { exact: true })).toBeVisible();
		await expect.element(page.getByText('LIVE', { exact: true })).toBeVisible();
	});
});
