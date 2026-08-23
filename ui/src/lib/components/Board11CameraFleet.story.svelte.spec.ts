import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board11CameraFleetStory from '../../../visual-harness/stories/Board11CameraFleetStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 11 Camera fleet story', () => {
	it('renders shared production rows in the exact Paper shell and fixed lanes', async () => {
		await page.viewport(1440, 800);
		const { container } = await render(Board11CameraFleetStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="cameras.desktop.fleet"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 624]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[64, 622],
			[1374, 622]
		]);
		const main = frame!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 436],
			[1374, 44],
			[1374, 58],
			[1374, 32]
		]);
		const rows = [...frame!.querySelectorAll<HTMLElement>('[data-fleet-row]')];
		expect(rows).toHaveLength(6);
		expect(rows.map(roundedSize)).toEqual(Array.from({ length: 6 }, () => [1334, 56]));
	});

	it('preserves mixed fleet evidence without inventing events, groups, or published variants', async () => {
		await page.viewport(1440, 800);
		const { container } = await render(Board11CameraFleetStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect.element(page.getByText('127 OF 127 SOURCES', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('DEGRADED · 14% frames dropped', { exact: true }).last())
			.toBeVisible();
		await expect
			.element(page.getByText('OFFLINE · Authentication failed', { exact: true }).last())
			.toBeVisible();
		await expect
			.element(page.getByText('VIRTUALISED · 56PX ROWS · RENDERS 24 AT A TIME', { exact: true }))
			.toBeVisible();
		expect(frame!.textContent).not.toContain('object-detect');
		expect(frame!.textContent).not.toContain('Front Door — annotated');
		expect(frame!.textContent).not.toContain('Group: Front of house');
		expect(frame!.textContent).not.toContain('Person ·');
	});
});
