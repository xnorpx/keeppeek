import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board21FirstRunStory from '../../../visual-harness/stories/Board21FirstRunStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 21 first-run and empty-state story', () => {
	it('renders the production owners in the exact Paper row geometry', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board21FirstRunStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="setup.desktop.first-run"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 785]);

		const panel = frame!.querySelector<HTMLElement>('[data-first-run-panel]');
		const emptyStates = frame!.querySelector<HTMLElement>('[data-first-run-empty-states]');
		expect(panel).not.toBeNull();
		expect(emptyStates).not.toBeNull();
		expect(roundedSize(panel!)).toEqual([708, 785]);
		expect(roundedSize(emptyStates!)).toEqual([708, 572]);
		expect([...panel!.children].map(roundedSize)).toEqual([
			[706, 189],
			[706, 515],
			[706, 79]
		]);
		expect([...emptyStates!.children].map(roundedSize)).toEqual([
			[708, 300],
			[708, 116],
			[708, 116]
		]);
	});

	it('keeps unavailable backend evidence explicit and all unsupported actions blocked', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board21FirstRunStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect.element(page.getByText('WRITE PROOF UNAVAILABLE', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('DETECTED FROM THIS BROWSER', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByText(/Server update required · keeppeek\.identity\.v1/))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Start the recorder' })).toBeDisabled();
		await expect.element(page.getByRole('button', { name: 'Registry unavailable' })).toBeDisabled();
		expect(frame!.textContent).not.toContain('WRITABLE');
		expect(frame!.textContent).not.toContain('DETECTED FROM THIS MACHINE');
		expect(frame!.textContent).not.toContain('0 EVENT SOURCES REGISTERED');
	});
});
