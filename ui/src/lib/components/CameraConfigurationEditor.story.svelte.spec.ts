import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board23CameraConfigurationStory from '../../../visual-harness/stories/Board23CameraConfigurationStory.svelte';

describe('Board 23 Camera configuration story', () => {
	it('renders the per-camera editor inside the exact Paper workspace', async () => {
		await page.viewport(1374, 900);
		const { container } = await render(Board23CameraConfigurationStory);
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="camera.desktop.configuration"]'
		);
		expect(frame).not.toBeNull();
		expect([
			Math.round(frame!.getBoundingClientRect().width),
			Math.round(frame!.getBoundingClientRect().height)
		]).toEqual([1374, 806]);
		await expect.element(page.getByRole('heading', { name: 'Edit camera settings' })).toBeVisible();
		await expect.element(page.getByLabelText('Display name')).toHaveValue('Front Door');
		await expect.element(page.getByLabelText('Recording mode')).toHaveValue('event-boost');
		await expect.element(page.getByRole('button', { name: 'Save camera settings' })).toBeEnabled();
		expect(frame!.textContent).not.toContain('write-only-password');
	});
});
