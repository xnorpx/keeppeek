import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board23CameraDefaultsStory from '../../../visual-harness/stories/Board23CameraDefaultsStory.svelte';

describe('Board 23 Camera Defaults story', () => {
	it('renders the production section inside the exact Paper content frame', async () => {
		await page.viewport(1374, 900);
		const { container } = await render(Board23CameraDefaultsStory);
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="settings.desktop.camera-defaults"]'
		);
		expect(frame).not.toBeNull();
		expect([
			Math.round(frame!.getBoundingClientRect().width),
			Math.round(frame!.getBoundingClientRect().height)
		]).toEqual([1374, 806]);

		const section = frame!.querySelector<HTMLElement>('#camera-defaults');
		expect(section).not.toBeNull();
		expect(Math.round(section!.getBoundingClientRect().width)).toBe(1310);
		await expect.element(page.getByText('42', { exact: true }).first()).toBeVisible();
		await expect
			.element(page.getByText('SHARED INHERITANCE NOT EXPOSED', { exact: true }))
			.toBeVisible();
		expect(frame!.textContent).not.toContain('admin');
	});
});
