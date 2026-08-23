import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board12AddCameraStory from '../../../visual-harness/stories/Board12AddCameraStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 12 desktop Add Camera story', () => {
	it('renders the production stream owner in the exact Paper wizard geometry', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board12AddCameraStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="cameras.desktop.add-wizard"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 663]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[300, 661],
			[1140, 661]
		]);
		const body = frame!.querySelector<HTMLElement>('[data-camera-wizard-step-body]');
		expect(body).not.toBeNull();
		expect([...body!.children].map(roundedSize)).toEqual([
			[1140, 107],
			[1140, 405],
			[1140, 76],
			[1140, 73]
		]);
		const streamCards = [...frame!.querySelectorAll<HTMLElement>('[data-camera-wizard-stream]')];
		expect(streamCards.map(roundedSize)).toEqual([
			[532, 357],
			[532, 357]
		]);
	});

	it('shows ONVIF candidates without claiming decoded media evidence before save', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board12AddCameraStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect
			.element(page.getByText('NOTHING SAVED UNTIL STEP 5', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('1 · Find & connect', { exact: true })).toBeVisible();
		await expect.element(page.getByText('ONVIF LOOKUP FROM STEP 1', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('ENDPOINT DECLARATION', { exact: true }).first())
			.toBeVisible();
		await expect.element(page.getByText('ONVIF REPORTED', { exact: true }).first()).toBeVisible();
		await expect
			.element(
				page.getByText('Decoded stream evidence is unavailable before save', { exact: true })
			)
			.toBeVisible();
		expect(frame!.textContent).not.toContain('DECODING');
		expect(frame!.textContent).not.toContain('FIRST KEYFRAME');
		expect(frame!.textContent).not.toContain('AUTHENTICATED');
		expect(frame!.textContent).not.toContain('write-only-password');
	});
});
