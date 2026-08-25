import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board30CameraDiagnosisStory from '../../../visual-harness/stories/Board30CameraDiagnosisStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 30 camera diagnosis story', () => {
	it('renders the shared diagnosis owner in the exact Paper shell and lane geometry', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board30CameraDiagnosisStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="health.desktop.camera-diagnosis"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 776]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[64, 774],
			[1374, 774]
		]);

		const owner = frame!.querySelector<HTMLElement>('[data-desktop-camera-diagnosis]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 722]
		]);
		const body = owner!.querySelector<HTMLElement>('[data-camera-diagnosis-body]');
		expect(body).not.toBeNull();
		expect([...body!.children].map(roundedSize)).toEqual([
			[840, 674],
			[464, 634]
		]);
	});

	it('keeps missing diagnosis evidence and WebRTC controls explicit', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board30CameraDiagnosisStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect
			.element(page.getByText('Camera transport is disconnected', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Retry now' })).toBeDisabled();
		await expect.element(page.getByRole('button', { name: 'Probe unavailable' })).toBeDisabled();
		await expect.element(page.getByRole('link', { name: 'Review settings' })).toBeVisible();
		expect(frame!.textContent).not.toContain('keeppeek.runtime-config.v1');
		await expect.element(page.getByText('MISSING', { exact: true }).last()).toBeVisible();
		expect(frame!.textContent).toContain('TRANSPORT');
		expect(frame!.textContent).toContain('DECODABLE');
		expect(frame!.textContent).not.toContain('2h 14m');
		expect(frame!.textContent).not.toContain('The hour before it stopped');
		expect(frame!.textContent).not.toContain('packet loss');
	});
});
