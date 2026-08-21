import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board07CameraPageStory from '../../../visual-harness/stories/Board07CameraPageStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 7 desktop Camera story', () => {
	it('renders the exact shell, context, anchor, content, and section lanes', async () => {
		await page.viewport(1440, 2200);
		const { container } = await render(Board07CameraPageStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="camera.desktop.details-ptz"]'
		);
		const owner = frame?.querySelector<HTMLElement>('[data-camera-paper-frame]');
		expect(frame).not.toBeNull();
		expect(owner).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 2059]);
		expect([...owner!.children].map(roundedSize)).toEqual([
			[64, 2057],
			[1374, 2057]
		]);
		const main = owner!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 2005]
		]);
		const body = main.children[1];
		expect([...body.children].map(roundedSize)).toEqual([
			[196, 2005],
			[1178, 2005]
		]);
		const content = body.children[1];
		expect([...content.children].map(roundedSize)).toEqual([
			[1130, 394],
			[1130, 154],
			[1130, 217],
			[1130, 263],
			[1130, 199],
			[1130, 292],
			[1130, 131],
			[1130, 111]
		]);
	});

	it('uses the shared nine-direction WebRTC PTZ owner without inventing settings evidence', async () => {
		await page.viewport(1440, 2200);
		const { container } = await render(Board07CameraPageStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		const ptz = frame?.querySelector<HTMLElement>('[data-camera-ptz-control="paper"]');
		expect(frame).not.toBeNull();
		expect(ptz).not.toBeNull();
		expect(roundedSize(ptz!)).toEqual([410, 394]);
		for (const name of [
			'Move up-left',
			'Tilt up',
			'Move up-right',
			'Pan left',
			'Stop PTZ',
			'Pan right',
			'Move down-left',
			'Tilt down',
			'Move down-right'
		]) {
			await expect.element(page.getByRole('button', { name, exact: true })).toBeEnabled();
		}
		expect(frame!.textContent).toContain(
			'Per-camera retention, mode, and inheritance are not returned.'
		);
		expect(frame!.textContent).toContain('publisher identity not returned');
		expect(frame!.textContent).not.toContain('object-detect');
		expect(frame!.textContent).not.toContain('30 days');
		expect(frame!.textContent).not.toContain('Shared camera login');
	});
});
