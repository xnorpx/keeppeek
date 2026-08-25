import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board26MobileHealthStory from '../../../visual-harness/stories/Board26MobileHealthStory.svelte';

describe('Board 26 mobile Health story', () => {
	it('renders the production overview in the exact Paper frame and lanes', async () => {
		await page.viewport(390, 900);
		const { container } = await render(Board26MobileHealthStory);
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="health.mobile.overview"]'
		);
		expect(frame).not.toBeNull();
		expect([
			Math.round(frame!.getBoundingClientRect().width),
			Math.round(frame!.getBoundingClientRect().height)
		]).toEqual([390, 844]);

		const overview = frame!.querySelector<HTMLElement>('[data-mobile-health-overview]');
		const primary = frame!.querySelector<HTMLElement>('[data-health-priority]');
		const navigation = frame!.querySelector<HTMLElement>('[data-shell-mobile-nav]');
		expect([
			Math.round(overview!.children[0].getBoundingClientRect().height),
			Math.round(overview!.children[1].getBoundingClientRect().height),
			Math.round(primary!.getBoundingClientRect().height),
			Math.round(navigation!.getBoundingClientRect().height)
		]).toEqual([52, 650, 196, 78]);
		await expect.element(page.getByText('Back Yard transport is disconnected')).toBeVisible();
		await expect.element(page.getByText('39 / 42', { exact: true })).not.toBeInTheDocument();
		await expect.element(page.getByRole('link', { name: 'Diagnose Back Yard' })).toBeVisible();
		expect(frame!.textContent).not.toContain('Mute 24h');
	});
});
