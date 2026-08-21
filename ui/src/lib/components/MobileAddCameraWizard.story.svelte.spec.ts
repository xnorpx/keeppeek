import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import type { MobileCameraWizardStage } from './MobileAddCameraWizard.svelte';
import Board25MobileAddCameraStory from '../../../visual-harness/stories/Board25MobileAddCameraStory.svelte';

async function renderStage(stage: MobileCameraWizardStage) {
	await page.viewport(390, 900);
	const { container } = await render(Board25MobileAddCameraStory, { props: { stage } });
	const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
	expect(frame).not.toBeNull();
	expect([
		Math.round(frame!.getBoundingClientRect().width),
		Math.round(frame!.getBoundingClientRect().height)
	]).toEqual([390, 844]);
	const wizard = frame!.querySelector<HTMLElement>('[data-mobile-camera-wizard]');
	expect(wizard).not.toBeNull();
	expect(
		[...wizard!.children].map((child) => Math.round(child.getBoundingClientRect().height))
	).toEqual([52, 660, 68]);
	return frame!;
}

describe('Board 25 mobile Add Camera stories', () => {
	it('renders find, discovery, credentials, and manual entry without writing', async () => {
		const frame = await renderStage('find-connect');
		await expect.element(page.getByText('192.168.1.71', { exact: true })).toBeVisible();
		await expect.element(page.getByLabelText('Username')).toHaveValue('admin');
		await expect.element(page.getByRole('button', { name: 'Connect' })).toBeVisible();
		expect(frame.textContent).toContain('WRITE-ONLY DRAFT');
	});

	it('renders stream declarations without claiming decoded evidence', async () => {
		const frame = await renderStage('streams');
		await expect.element(page.getByText('PROBE UNAVAILABLE', { exact: true })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Review' })).toBeVisible();
		expect(frame.textContent).not.toContain('DECODING');
		expect(frame.textContent).not.toContain('TESTED');
	});

	it('renders final review as the only save stage without retention claims', async () => {
		const frame = await renderStage('review');
		await expect.element(page.getByLabelText('CAMERA NAME')).toHaveValue('Side Gate');
		await expect
			.element(page.getByText('Retention impact unavailable', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Save camera' })).toBeVisible();
		expect(frame.textContent).toContain('Saving is the first configuration write.');
		expect(frame.textContent).not.toContain('Connection and both streams passed.');
	});
});
