import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board04KeepTimelineStory from '../../../visual-harness/stories/Board04KeepTimelineStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 4 Keep timeline story', () => {
	it('renders the exact shell, player, and right-edge timeline lanes', async () => {
		await page.viewport(1280, 800);
		const { container } = await render(Board04KeepTimelineStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="keep.desktop.timeline-anatomy"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1280, 720]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[64, 718],
			[818, 718],
			[396, 718]
		]);
		const player = frame!.children[1];
		expect([...player.children].map(roundedSize)).toEqual([
			[818, 52],
			[818, 666]
		]);
		const timeline = frame!.querySelector<HTMLElement>('[aria-label="Recording timeline"]');
		expect(timeline).not.toBeNull();
		expect([...timeline!.children].slice(0, 3).map(roundedSize)).toEqual([
			[395, 52],
			[395, 46],
			[395, 620]
		]);
	});

	it('renders returned availability, explicit gaps, duration activity, events, live edge, and playhead', async () => {
		await page.viewport(1280, 800);
		const { container } = await render(Board04KeepTimelineStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();
		expect(frame!.querySelectorAll('[data-timeline-availability]')).toHaveLength(5);
		expect(frame!.querySelectorAll('[data-timeline-gap]')).toHaveLength(5);
		expect(frame!.querySelectorAll('[data-timeline-activity]')).toHaveLength(6);
		expect(frame!.querySelectorAll('[data-timeline-event]')).toHaveLength(6);
		expect(frame!.querySelectorAll('[data-timeline-event-marker]')).toHaveLength(6);
		await expect.element(page.getByText('LIVE', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Playback position at 06:37/i }))
			.toBeVisible();
		expect(frame!.textContent).not.toContain('object-detect');
		expect(frame!.textContent).not.toContain('rev 2');
		expect(frame!.textContent).not.toContain('6 frames');
	});
});
