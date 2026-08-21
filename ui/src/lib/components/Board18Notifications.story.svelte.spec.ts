import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board18NotificationsStory from '../../../visual-harness/stories/Board18NotificationsStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 18 Notifications story', () => {
	it('renders the production owner in the exact Paper shell and band geometry', async () => {
		await page.viewport(1440, 1200);
		const { container } = await render(Board18NotificationsStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="settings.desktop.notifications"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 1075]);

		const owner = frame!.querySelector<HTMLElement>('[data-notifications-paper-frame]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[64, 1073],
			[1374, 1073]
		]);
		const main = owner!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 1021]
		]);
		const bands = [...owner!.querySelectorAll<HTMLElement>('[data-notification-band]')];
		expect(bands.map(roundedSize)).toEqual([
			[1310, 195],
			[1310, 288],
			[1310, 430]
		]);
		expect(owner!.querySelectorAll('[data-notification-channel]')).toHaveLength(4);
		expect(owner!.querySelectorAll('[data-notification-rule-evidence]')).toHaveLength(4);
	});

	it('keeps all channel, rule, quiet-hour, and delivery evidence unavailable', async () => {
		await page.viewport(1440, 1200);
		const { container } = await render(Board18NotificationsStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect
			.element(page.getByText(/Server update required · keeppeek\.rules\.v1/).first())
			.toBeVisible();
		await expect
			.element(page.getByText('FIRING HISTORY UNAVAILABLE', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('NO QUIET-HOURS CONTRACT', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Delivery history unavailable', { exact: true }))
			.toBeVisible();
		for (const absent of [
			'Pushover',
			'Last delivered 41m ago',
			'Permission granted',
			'FIRED 219 TIMES IN THE LAST 7 DAYS',
			'3,190',
			'22:00 – 06:30 · EUROPE/STOCKHOLM',
			'Person at Front Door'
		]) {
			expect(frame!.textContent).not.toContain(absent);
		}
	});
});
