import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board27MobileAdministrationStory from '../../../visual-harness/stories/Board27MobileAdministrationStory.svelte';

type State = 'access' | 'index';

async function renderState(state: State) {
	await page.viewport(390, 900);
	const { container } = await render(Board27MobileAdministrationStory, { props: { state } });
	const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
	expect(frame).not.toBeNull();
	const frameBounds = frame!.getBoundingClientRect();
	expect([Math.round(frameBounds.width), Math.round(frameBounds.height)]).toEqual([390, 844]);
	return { container, frame: frame! };
}

describe('Board 27 mobile administration story', () => {
	it('renders the production index in the exact Paper frame and row lanes', async () => {
		const { frame } = await renderState('index');

		const statusBar = frame!.querySelector<HTMLElement>('[data-mobile-status-bar]');
		const header = frame!.querySelector<HTMLElement>('[data-mobile-settings-header]');
		const primaryNavigation = frame!.querySelector<HTMLElement>('[data-shell-mobile-nav]');
		expect([
			Math.round(statusBar!.getBoundingClientRect().height),
			Math.round(header!.getBoundingClientRect().height),
			Math.round(primaryNavigation!.getBoundingClientRect().height)
		]).toEqual([62, 52, 78]);

		const settingsLinks = [
			...frame!.querySelectorAll<HTMLElement>('[aria-label="Settings sections"] a')
		];
		expect(settingsLinks).toHaveLength(9);
		expect(settingsLinks.map((link) => Math.round(link.getBoundingClientRect().height))).toEqual([
			52, 52, 52, 52, 52, 46, 46, 46, 46
		]);
	});

	it('renders unavailable identity evidence without Paper fixture people or tokens', async () => {
		const { frame } = await renderState('access');
		expect(
			[...frame.children].map((child) => Math.round(child.getBoundingClientRect().height))
		).toEqual([62, 52, 660, 68]);
		await expect
			.element(page.getByText('Identity directory unavailable', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByText('Token registry unavailable', { exact: true }))
			.toBeVisible();
		for (const unsupportedFixtureText of ['Marcus', 'Anna', 'Workshop tablet', 'object-detect']) {
			expect(frame.textContent).not.toContain(unsupportedFixtureText);
		}
		expect(frame.textContent).toContain('Server update required · keeppeek.identity.v1');
	});
});
