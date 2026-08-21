import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board22MobileMediaStory from '../../../visual-harness/stories/Board22MobileMediaStory.svelte';

async function renderState(state: 'events' | 'keep' | 'peek', lanes: number[]) {
	await page.viewport(390, 900);
	const { container } = await render(Board22MobileMediaStory, { props: { state } });
	const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
	expect(frame).not.toBeNull();
	expect([
		Math.round(frame!.getBoundingClientRect().width),
		Math.round(frame!.getBoundingClientRect().height)
	]).toEqual([390, 844]);
	expect(
		[...frame!.children].map((child) => Math.round(child.getBoundingClientRect().height))
	).toEqual(lanes);
	return frame!;
}

describe('Board 22 mobile media stories', () => {
	it('renders the production Peek tile geometry', async () => {
		const frame = await renderState('peek', [62, 50, 652, 78]);
		expect(frame.querySelectorAll('[data-peek-camera]')).toHaveLength(7);
		expect(
			Array.from(frame.querySelectorAll<HTMLElement>('[data-peek-camera]'))
				.slice(0, 3)
				.map((tile) => [
					Math.round(tile.getBoundingClientRect().width),
					Math.round(tile.getBoundingClientRect().height)
				])
		).toEqual([
			[358, 201],
			[174, 110],
			[174, 110]
		]);
	});

	it('renders the production vertical Keep timeline strip', async () => {
		const frame = await renderState('keep', [62, 50, 220, 434, 78]);
		await expect
			.element(page.getByRole('region', { name: 'Recording timeline', exact: true }))
			.toBeVisible();
		expect(frame.querySelectorAll('[data-timeline-event]').length).toBeGreaterThan(0);
	});

	it('renders the production Event hero and fixed rows', async () => {
		const frame = await renderState('events', [62, 50, 88, 564, 78]);
		const cards = Array.from(frame.querySelectorAll<HTMLElement>('[data-event-card]'));
		expect(cards.map((card) => Math.round(card.getBoundingClientRect().height))).toEqual([
			256, 78, 78, 78
		]);
		await expect.element(page.getByText('NO IMAGE', { exact: true }).first()).toBeVisible();
	});
});
