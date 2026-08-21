import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board20AppearanceSystemStory from '../../../visual-harness/stories/Board20AppearanceSystemStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 20 Appearance, System, and Logs story', () => {
	it('renders all three production panels in the exact Paper geometry', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board20AppearanceSystemStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="settings.desktop.appearance-system-logs"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 581]);

		const panels = [...frame!.querySelectorAll<HTMLElement>('[data-settings-panel]')];
		expect(panels.map(roundedSize)).toEqual([
			[466, 548],
			[466, 581],
			[468, 391]
		]);
		expect([...panels[0].children].map(roundedSize)).toEqual([
			[420, 24],
			[420, 98],
			[420, 54],
			[420, 54],
			[420, 96],
			[420, 40],
			[420, 40]
		]);
		expect([...panels[1].children].map(roundedSize)).toEqual([
			[420, 24],
			[420, 90],
			[420, 200],
			[420, 34],
			[420, 123]
		]);
		expect([...panels[2].children].map(roundedSize)).toEqual([
			[422, 24],
			[422, 28],
			[422, 159],
			[422, 34],
			[422, 36]
		]);
	});

	it('keeps absent server preferences and commands unavailable', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board20AppearanceSystemStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect.element(page.getByText('BROWSER ONLY', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('group', { name: 'Clock preference unavailable' }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Check for updates' })).toBeDisabled();
		await expect.element(page.getByRole('button', { name: 'Back up config' })).toBeDisabled();
		await expect.element(page.getByRole('button', { name: 'Erase', exact: true })).toBeDisabled();
		await expect.element(page.getByRole('button', { name: 'Restart', exact: true })).toBeEnabled();
		await expect
			.element(page.getByRole('link', { name: 'Open logs' }))
			.toHaveAttribute('href', '/settings/logs');
		await expect
			.element(page.getByRole('link', { name: 'Open health' }))
			.toHaveAttribute('href', '/system-health');
		expect(frame!.textContent).not.toContain('UTC+2');
		expect(frame!.textContent).not.toContain('Stable');
		expect(frame!.textContent).not.toContain('~/.config/keeppeek.toml');
		expect(frame!.textContent).not.toContain('Download diagnostics');
	});
});
