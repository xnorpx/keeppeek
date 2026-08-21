import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board08LayoutEditorStory from '../../../visual-harness/stories/Board08LayoutEditorStory.svelte';
import Board08LayoutRegistryStory from '../../../visual-harness/stories/Board08LayoutRegistryStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 8 Layout editor story', () => {
	it('renders the shared editor in the exact Paper shell and body lanes', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board08LayoutEditorStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="peek.desktop.layout-editor"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 840]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[64, 838],
			[1374, 838]
		]);

		const editor = frame!.querySelector<HTMLElement>('[data-peek-layout-editor]');
		const body = editor?.querySelector<HTMLElement>('[data-peek-layout-body]');
		expect(editor).not.toBeNull();
		expect(body).not.toBeNull();
		expect([...editor!.children].map(roundedSize)).toEqual([
			[1374, 56],
			[1374, 784]
		]);
		expect([...body!.children].map(roundedSize)).toEqual([
			[1054, 784],
			[320, 784]
		]);
		expect([...body!.lastElementChild!.children].map(roundedSize)).toEqual([
			[319, 121],
			[319, 196],
			[319, 467]
		]);
	});

	it('keeps layout persistence and registry evidence unavailable', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board08LayoutEditorStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();
		expect(frame!.querySelectorAll('[data-peek-layout-item]')).toHaveLength(3);
		await expect.element(page.getByText('3 OF 6 PLACED', { exact: true })).toBeVisible();
		const done = page.getByRole('button', { name: 'Done', exact: true });
		await expect.element(done).toBeDisabled();
		expect(frame!.textContent).not.toContain('127 PLACED');
		expect(frame!.textContent).not.toContain('Perimeter night');
		expect(frame!.textContent).not.toContain('Everything');
	});

	it('renders the blocked server-layout composition without fixture identities', async () => {
		await page.viewport(1440, 600);
		const { container } = await render(Board08LayoutRegistryStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="peek.desktop.layout-registry"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 396]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[706, 351],
			[706, 351]
		]);
		await expect.element(page.getByText('Server layout registry unavailable')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Delete layout' })).toBeDisabled();
		expect(frame!.textContent).not.toContain('Front of house');
		expect(frame!.textContent).not.toContain('Perimeter night');
		expect(frame!.textContent).not.toContain('127 CAMERAS');
	});
});
