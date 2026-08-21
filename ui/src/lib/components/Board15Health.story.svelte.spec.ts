import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board15HealthStory from '../../../visual-harness/stories/Board15HealthStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 15 server and client Health story', () => {
	it('renders the shared overview in the exact Paper shell and band geometry', async () => {
		await page.viewport(1440, 1400);
		const { container } = await render(Board15HealthStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="health.desktop.overview"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 1302]);

		const owner = frame!.querySelector<HTMLElement>('[data-desktop-health-overview]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[64, 1300],
			[1374, 1300]
		]);
		const main = owner!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 1248]
		]);
		const bands = [...owner!.querySelectorAll<HTMLElement>('[data-health-overview-band]')];
		expect(bands.map(roundedSize)).toEqual([
			[1310, 130],
			[1310, 246],
			[1310, 130],
			[1310, 248],
			[1310, 326]
		]);
		expect(owner!.querySelectorAll('[data-health-stream-row]')).toHaveLength(4);
	});

	it('uses canonical WebRTC health control and only the allowed HTTP evidence routes', async () => {
		await page.viewport(1440, 1400);
		const { container } = await render(Board15HealthStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect.element(page.getByText('HealthCommand', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('WebRTC · protobuf snapshot', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('/metrics', { exact: true })).toBeVisible();
		await expect.element(page.getByText('/logs', { exact: true })).toBeVisible();
		expect(frame!.textContent).not.toContain('GET /health');
		expect(frame!.textContent).not.toContain('Mute for 24h');
		expect(frame!.textContent).not.toContain('doorbell-bridge');
	});
});
