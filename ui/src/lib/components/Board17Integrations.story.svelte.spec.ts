import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board17IntegrationsStory from '../../../visual-harness/stories/Board17IntegrationsStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 17 Integrations story', () => {
	it('renders the production owner in the exact Paper shell and integration bands', async () => {
		await page.viewport(1440, 1000);
		const { container } = await render(Board17IntegrationsStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="settings.desktop.integrations"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 869]);

		const owner = frame!.querySelector<HTMLElement>('[data-integrations-paper-frame]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].slice(0, 2).map(roundedSize)).toEqual([
			[64, 867],
			[1374, 867]
		]);
		const main = owner!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 815]
		]);
		const bands = [...owner!.querySelectorAll<HTMLElement>('[data-integration-band]')];
		expect(bands.map(roundedSize)).toEqual([
			[1310, 205],
			[1310, 236],
			[1310, 278]
		]);
		expect([...bands[2].children].map(roundedSize)).toEqual([
			[645, 270],
			[645, 278]
		]);
	});

	it('shows real operational endpoints without inventing configured integrations', async () => {
		await page.viewport(1440, 1000);
		const { container } = await render(Board17IntegrationsStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect.element(page.getByText('/metrics', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Open metrics' }))
			.toHaveAttribute('href', '/metrics');
		await expect
			.element(page.getByRole('link', { name: 'Health' }))
			.toHaveAttribute('href', '/system-health');
		await expect
			.element(page.getByRole('link', { name: 'Logs' }))
			.toHaveAttribute('href', '/settings/logs');
		for (const absent of [
			'https://home.lan:8123',
			'mqtt://home.lan:1883',
			'https://automation.lan/hooks/kp',
			'https://ops.example.com/kp',
			'1,402 MESSAGES TODAY',
			'SCRAPED 11s AGO'
		]) {
			expect(frame!.textContent).not.toContain(absent);
		}
		expect(frame!.textContent).not.toMatch(/kp_ha_[a-z0-9]+/);
	});
});
