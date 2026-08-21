import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board09KeepModesStory from '../../../visual-harness/stories/Board09KeepModesStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 9 Keep mode stories', () => {
	it('renders the shared Stories owner in the exact authored frame', async () => {
		await page.viewport(800, 600);
		const { container } = await render(Board09KeepModesStory, { state: 'stories' });
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="keep.desktop.stories"]'
		);
		const panel = frame?.querySelector<HTMLElement>('[data-keep-stories-panel]');
		expect(frame).not.toBeNull();
		expect(panel).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([467, 413]);
		expect(roundedSize(panel!)).toEqual([467, 413]);
		expect([...panel!.children].map(roundedSize)).toEqual([
			[465, 48],
			[465, 271]
		]);
		expect(roundedSize(panel!.querySelector('[data-keep-story]')!)).toEqual([433, 156]);
		expect(frame!.textContent).toContain('Summary and additional frames were not reported');
		expect(frame!.textContent).not.toContain('delivery van');
		expect(frame!.textContent).not.toContain('OBJECT-DETECT');
	});

	it('renders footage-backed Calendar days without claiming a retention cause', async () => {
		await page.viewport(800, 600);
		const { container } = await render(Board09KeepModesStory, { state: 'calendar' });
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="keep.desktop.calendar"]'
		);
		const panel = frame?.querySelector<HTMLElement>('[data-keep-calendar-panel]');
		expect(frame).not.toBeNull();
		expect(panel).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([467, 413]);
		expect([...panel!.children].map(roundedSize)).toEqual([
			[465, 48],
			[465, 204]
		]);
		expect(panel!.querySelectorAll('[data-calendar-date]')).toHaveLength(14);
		expect(
			panel!.querySelector<HTMLButtonElement>('[data-calendar-date="2026-08-18"]')?.disabled
		).toBe(false);
		expect(
			panel!.querySelector<HTMLButtonElement>('[data-calendar-date="2026-08-14"]')?.disabled
		).toBe(true);
		expect(frame!.textContent).toContain('cause not reported');
		expect(frame!.textContent).not.toContain('retention reclaimed');
	});

	it('renders the fail-closed Export draft in the exact authored frame', async () => {
		await page.viewport(800, 600);
		const { container } = await render(Board09KeepModesStory, { state: 'export' });
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="keep.desktop.export-gated"]'
		);
		const panel = frame?.querySelector<HTMLElement>('[data-keep-export]');
		expect(frame).not.toBeNull();
		expect(panel).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([467, 413]);
		expect(roundedSize(panel!)).toEqual([467, 413]);
		expect([...panel!.children].map(roundedSize)).toEqual([
			[465, 48],
			[465, 363]
		]);
		await expect
			.element(page.getByRole('textbox', { name: 'FROM', exact: true }))
			.toHaveValue('06:11:48');
		await expect
			.element(page.getByRole('textbox', { name: 'TO', exact: true }))
			.toHaveValue('06:14:20');
		await expect
			.element(page.getByText('Server update required · keeppeek.media-export.v1'))
			.toBeVisible();
	});

	it('renders four indexed lanes on one exact shared clock', async () => {
		await page.viewport(1440, 600);
		const { container } = await render(Board09KeepModesStory, { state: 'swimlanes' });
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="keep.desktop.swimlanes"]'
		);
		const owner = frame?.querySelector<HTMLElement>('[data-keep-swimlanes-owner]');
		expect(frame).not.toBeNull();
		expect(owner).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 363]);
		expect([...owner!.children].map(roundedSize)).toEqual([
			[1440, 34],
			[1440, 212],
			[1440, 36]
		]);
		expect(frame!.querySelectorAll('[data-swimlane]')).toHaveLength(4);
		expect([...frame!.querySelectorAll<HTMLElement>('[data-swimlane]')].map(roundedSize)).toEqual(
			Array.from({ length: 4 }, () => [1438, 44])
		);
		expect(frame!.querySelector('[data-swimlane-gap]')).not.toBeNull();
		expect(roundedSize(frame!.querySelector('[data-swimlane-playhead]')!)).toEqual([2, 210]);
	});
});
