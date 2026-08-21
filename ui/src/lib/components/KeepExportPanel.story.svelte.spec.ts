import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import ExportLifecycleStory from '../../../visual-harness/stories/ExportLifecycleStory.svelte';

describe('Board 29 export lifecycle story', () => {
	it('renders all four production states inside the Paper frame', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(ExportLifecycleStory);

		await expect.element(page.getByText('Your file is ready', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('The disk filled while writing', { exact: true }))
			.toBeVisible();

		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="keep.desktop.export-lifecycle"]'
		);
		expect(frame).not.toBeNull();
		const frameBounds = frame!.getBoundingClientRect();
		expect([Math.round(frameBounds.width), Math.round(frameBounds.height)]).toEqual([1440, 369]);

		const panels = [...container.querySelectorAll<HTMLElement>('[data-keep-export]')];
		expect(panels.map((panel) => panel.dataset.exportStatus)).toEqual([
			'running',
			'ready',
			'partial',
			'failed'
		]);
		expect(panels).toHaveLength(4);
		const cardBounds = panels.map((panel) => {
			const bounds = panel.getBoundingClientRect();
			expect(Math.round(bounds.width)).toBe(342);
			return {
				status: panel.dataset.exportStatus,
				height: Math.round(bounds.height),
				bottom: bounds.bottom
			};
		});
		if (cardBounds.some((bounds) => bounds.bottom > frameBounds.bottom)) {
			throw new Error(`Board 29 card clipping: ${JSON.stringify(cardBounds)}`);
		}

		const expectedLanes: Record<string, Array<[number, number]>> = {
			running: [
				[20, 42],
				[78, 26],
				[120, 80],
				[216, 32]
			],
			ready: [
				[20, 42],
				[78, 100],
				[194, 100]
			],
			partial: [
				[20, 42],
				[78, 33],
				[127, 80],
				[223, 32]
			],
			failed: [
				[20, 42],
				[78, 78],
				[172, 54],
				[242, 32],
				[290, 14]
			]
		};
		for (const panel of panels) {
			const status = panel.dataset.exportStatus;
			expect(status).toBeTruthy();
			const content = panel.querySelector<HTMLElement>('[data-export-job]');
			expect(content).not.toBeNull();
			const contentBounds = content!.getBoundingClientRect();
			expect(
				[...content!.children].map((child) => {
					const bounds = child.getBoundingClientRect();
					return [Math.round(bounds.y - contentBounds.y), Math.round(bounds.height)] as [
						number,
						number
					];
				})
			).toEqual(expectedLanes[status!]);
		}
	});
});
