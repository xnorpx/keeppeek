import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import type { MobileCameraMode } from './MobileCameraPage.svelte';
import Board24MobileCameraStory from '../../../visual-harness/stories/Board24MobileCameraStory.svelte';

async function renderMode(mode: MobileCameraMode) {
	await page.viewport(390, 900);
	const { container } = await render(Board24MobileCameraStory, { props: { mode } });
	const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
	expect(frame).not.toBeNull();
	expect([
		Math.round(frame!.getBoundingClientRect().width),
		Math.round(frame!.getBoundingClientRect().height)
	]).toEqual([390, 844]);
	return frame!;
}

describe('Board 24 mobile Camera stories', () => {
	it('renders the live camera lanes without fabricating recent events or audio controls', async () => {
		const frame = await renderMode('live');
		const pageOwner = frame.querySelector<HTMLElement>('[data-mobile-camera-page="live"]');
		expect(
			[...pageOwner!.children].map((child) => Math.round(child.getBoundingClientRect().height))
		).toEqual([52, 652, 76]);
		await expect
			.element(page.getByText('Recent event evidence unavailable', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'PTZ', exact: true })).toBeEnabled();
		await expect.element(page.getByRole('button', { name: 'Talk', exact: true })).toBeDisabled();
	});

	it('renders shared WebRTC PTZ controls and server presets', async () => {
		const frame = await renderMode('ptz');
		const pageOwner = frame.querySelector<HTMLElement>('[data-mobile-camera-page="ptz"]');
		expect(
			[...pageOwner!.children].map((child) => Math.round(child.getBoundingClientRect().height))
		).toEqual([52, 728]);
		await expect.element(page.getByRole('button', { name: 'Pan right' })).toBeEnabled();
		await expect.element(page.getByRole('button', { name: 'Front step' })).toBeVisible();
	});

	it('renders current settings without inherited-secret or save claims', async () => {
		const frame = await renderMode('settings');
		const pageOwner = frame.querySelector<HTMLElement>('[data-mobile-camera-page="settings"]');
		expect(
			[...pageOwner!.children].map((child) => Math.round(child.getBoundingClientRect().height))
		).toEqual([52, 674, 54]);
		expect(frame.textContent).toContain('Inheritance unknown');
		expect(frame.textContent).toContain('Per-camera retention and inheritance are not exposed.');
		expect(frame.textContent).not.toContain('Save');
	});
});
