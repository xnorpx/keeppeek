import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board33StatesStory from '../../../visual-harness/stories/Board33StatesStory.svelte';

type State =
	'applying' | 'cold-seek' | 'discovery' | 'first-keyframe' | 'fleet-skeleton' | 'no-results';

async function renderState(state: State, width: number, height: number) {
	await page.viewport(1440, 900);
	const { container } = await render(Board33StatesStory, { props: { state } });
	await document.fonts.ready;
	const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
	expect(frame).not.toBeNull();
	const bounds = frame!.getBoundingClientRect();
	expect([Math.round(bounds.width), Math.round(bounds.height)]).toEqual([width, height]);
	return { container, frame: frame!, bounds };
}

describe('Board 33 waiting and empty stories', () => {
	it('renders the negotiated first-keyframe frame', async () => {
		const { container } = await renderState('first-keyframe', 462, 172);
		const state = container.querySelector<HTMLElement>('[data-first-frame-state]');
		expect(state?.dataset.firstFrameState).toBe('waiting');
		expect(state?.dataset.firstFrameElapsedMs).toBe('400');
		await expect.element(page.getByText('CONNECTING', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Negotiated · waiting for a keyframe · 0.4s', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('Alley', { exact: true })).toBeVisible();
	});

	it('renders the cold-seek frame without replacing the current image', async () => {
		const { container, bounds } = await renderState('cold-seek', 462, 172);
		const state = container.querySelector<HTMLElement>('[data-cold-seek]');
		expect(state?.dataset.coldSeekElapsedMs).toBe('1200');
		await expect.element(page.getByLabelText('14 Aug · 22:41:07', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Reading from the long-term tier · 1.2s', { exact: true }))
			.toBeVisible();
		const timestamp = state?.querySelector<HTMLElement>('p:first-child')?.getBoundingClientRect();
		expect(timestamp).not.toBeUndefined();
		expect([
			Math.round(timestamp!.x - bounds.x),
			Math.round(timestamp!.y - bounds.y),
			Math.round(timestamp!.width),
			Math.round(timestamp!.height)
		]).toEqual([122, 50, 218, 32]);
	});

	it('renders evidence-backed discovery progress', async () => {
		const { container, bounds } = await renderState('discovery', 462, 172);
		await expect.element(page.getByText('7', { exact: true })).toBeVisible();
		await expect.element(page.getByText('143 of 200 probes sent', { exact: true })).toBeVisible();
		const progress = container.querySelector<HTMLElement>('[role="progressbar"]');
		expect(progress?.getAttribute('aria-valuenow')).toBe('3200');
		const progressBounds = progress?.getBoundingClientRect();
		expect(progressBounds).not.toBeUndefined();
		expect([
			Math.round(progressBounds!.x - bounds.x),
			Math.round(progressBounds!.y - bounds.y),
			Math.round(progressBounds!.width),
			Math.round(progressBounds!.height)
		]).toEqual([18, 99, 426, 4]);
	});

	it('renders the constraining no-results clause and smallest loosening', async () => {
		await renderState('no-results', 462, 238);
		await expect.element(page.getByText('confidence:>0.9', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('No vehicles on Workshop that day', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Drop to >0.5 · 4 results' }))
			.toBeVisible();
	});

	it('renders confirmed values while an update is applying', async () => {
		await renderState('applying', 462, 238);
		await expect.element(page.getByText('Transport', { exact: true })).toBeVisible();
		await expect.element(page.getByText('TCP', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Applying…', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Fields locked until it lands', { exact: true }))
			.toBeVisible();
	});

	it('renders three stable non-shimmer fleet lanes', async () => {
		const { container } = await renderState('fleet-skeleton', 462, 238);
		const state = container.querySelector<HTMLElement>('[data-fleet-skeleton]');
		expect(state?.querySelectorAll('.h-14')).toHaveLength(3);
		expect(state?.querySelectorAll('[data-slot="skeleton"]')).toHaveLength(0);
		await expect
			.element(page.getByText('Reading the catalog · 42 cameras', { exact: true }))
			.toBeVisible();
	});

	it('loads the exact Paper font families', async () => {
		await renderState('applying', 462, 238);
		expect(
			document.fonts.check('600 16px Archivo') && document.fonts.check('400 16px "IBM Plex Mono"')
		).toBe(true);
	});
});
