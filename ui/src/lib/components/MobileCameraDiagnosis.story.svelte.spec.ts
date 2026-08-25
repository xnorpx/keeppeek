import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board26MobileDiagnosisStory from '../../../visual-harness/stories/Board26MobileDiagnosisStory.svelte';

async function renderState(state: 'issue' | 'stream') {
	await page.viewport(390, 900);
	const { container } = await render(Board26MobileDiagnosisStory, { props: { state } });
	const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
	expect(frame).not.toBeNull();
	expect([
		Math.round(frame!.getBoundingClientRect().width),
		Math.round(frame!.getBoundingClientRect().height)
	]).toEqual([390, 844]);
	const diagnosis = frame!.querySelector<HTMLElement>('[data-mobile-camera-diagnosis]');
	expect(diagnosis).not.toBeNull();
	expect(
		[...diagnosis!.children].map((child) => Math.round(child.getBoundingClientRect().height))
	).toEqual([52, 660, 68]);
	return frame!;
}

describe('Board 26 mobile diagnosis stories', () => {
	it('renders current offline evidence without gap-start or retry claims', async () => {
		const frame = await renderState('issue');
		await expect
			.element(page.getByRole('heading', { name: 'Camera transport is disconnected' }))
			.toBeVisible();
		await expect.element(page.getByText('27', { exact: true })).toBeVisible();
		expect(frame.textContent).toContain('transport_disconnected');
		expect(frame.textContent).toContain('Recording progress MISSING');
		expect(frame.textContent).toContain('Retry unavailable');
		expect(frame.textContent).not.toContain('NO FOOTAGE SINCE');
		expect(frame.textContent).not.toContain('18s');
	});

	it('renders current stream counters without fabricated loss history or confidence', async () => {
		const frame = await renderState('stream');
		expect(frame.textContent).toContain('184,000 drops observed');
		await expect.element(page.getByText('History unavailable', { exact: true })).toBeVisible();
		expect(frame.textContent).toContain('NO CAUSAL CONFIDENCE');
		expect(frame.textContent).toContain('Test TCP transport');
		expect(frame.textContent).toContain('SHIPS · CAMERA WRITE');
		expect(frame.textContent).not.toContain('LOSS 24H');
		expect(frame.textContent).not.toContain('HIGH CONFIDENCE');
	});
});
