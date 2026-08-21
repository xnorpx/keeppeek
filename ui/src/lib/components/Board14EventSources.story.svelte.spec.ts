import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board14EventSourcesStory from '../../../visual-harness/stories/Board14EventSourcesStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 14 Event Sources story', () => {
	it('renders the production owner in the exact Paper shell and content bands', async () => {
		await page.viewport(1440, 1100);
		const { container } = await render(Board14EventSourcesStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="settings.desktop.event-sources"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 1048]);

		const owner = frame!.querySelector<HTMLElement>('[data-event-sources-paper-frame]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[64, 1046],
			[1374, 1046]
		]);
		const main = owner!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 994]
		]);
		expect([...main.children[1].children].map(roundedSize)).toEqual([
			[240, 994],
			[1134, 994]
		]);
		const bands = [...owner!.querySelectorAll<HTMLElement>('[data-event-source-band]')];
		expect(bands.map(roundedSize)).toEqual([
			[1070, 84],
			[1070, 337],
			[1070, 461]
		]);
		expect(owner!.querySelectorAll('[data-event-origin]')).toHaveLength(2);
	});

	it('shows catalog and stored-field evidence without inventing publisher administration', async () => {
		await page.viewport(1440, 1100);
		const { container } = await render(Board14EventSourcesStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect.element(page.getByText('1,402', { exact: true })).toBeVisible();
		await expect.element(page.getByText('ALL-TIME CATALOG EVENTS', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Source registry unavailable', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('camera', { exact: true })).toBeVisible();
		await expect.element(page.getByText('keeppeek', { exact: true })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Register a source' })).toBeDisabled();
		for (const absent of [
			'object-detect',
			'doorbell-bridge',
			'1,402 EVENTS INGESTED TODAY',
			'CONNECTED 6d 04h',
			'CREATED 12 JUN'
		]) {
			expect(frame!.textContent).not.toContain(absent);
		}
		expect(frame!.textContent).not.toMatch(/kp_[a-z0-9]+/);
	});
});
