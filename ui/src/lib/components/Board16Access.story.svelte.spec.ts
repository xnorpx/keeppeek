import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board16AccessStory from '../../../visual-harness/stories/Board16AccessStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 16 Access and Roles story', () => {
	it('renders the production owner in the exact Paper shell and band geometry', async () => {
		await page.viewport(1440, 1300);
		const { container } = await render(Board16AccessStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="settings.desktop.access"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 1249]);

		const owner = frame!.querySelector<HTMLElement>('[data-access-paper-frame]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[64, 1247],
			[1374, 1247]
		]);
		const main = owner!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 1195]
		]);
		const bands = [...owner!.querySelectorAll<HTMLElement>('[data-access-band]')];
		expect(bands.map(roundedSize)).toEqual([
			[1310, 102],
			[1310, 416],
			[1310, 395],
			[1310, 142]
		]);
		expect(owner!.querySelectorAll('[data-access-permission]')).toHaveLength(8);
		expect([...bands[2].children].map(roundedSize)).toEqual([
			[645, 274],
			[645, 395]
		]);
	});

	it('shows target policy without inventing identity, token, session, or audit records', async () => {
		await page.viewport(1440, 1300);
		const { container } = await render(Board16AccessStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect
			.element(page.getByText(/Server update required · keeppeek\.identity\.v1/).first())
			.toBeVisible();
		await expect
			.element(page.getByText('Identity directory unavailable', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByText('Token registry unavailable', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('AUDIT TRAIL UNAVAILABLE', { exact: true })).toBeVisible();
		expect(frame!.querySelectorAll('[aria-label="Administrator target allows"]')).toHaveLength(8);
		expect(frame!.querySelectorAll('[aria-label="User target allows"]')).toHaveLength(4);
		expect(frame!.querySelectorAll('[aria-label="User target excludes"]')).toHaveLength(4);
		for (const absent of [
			'Marcus',
			'Anna',
			'Workshop tablet',
			'Front desk',
			'Home Assistant card',
			'object-detect',
			'Metrics collector',
			'doorbell-bridge'
		]) {
			expect(frame!.textContent).not.toContain(absent);
		}
		expect(frame!.querySelectorAll('input[type="password"]')).toHaveLength(0);
	});
});
