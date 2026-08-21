import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board13StorageRetentionStory from '../../../visual-harness/stories/Board13StorageRetentionStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 13 Storage and Retention story', () => {
	it('renders the production owner in the exact Paper shell and five content bands', async () => {
		await page.viewport(1440, 1250);
		const { container } = await render(Board13StorageRetentionStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="settings.desktop.storage-retention"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 1163]);

		const owner = frame!.querySelector<HTMLElement>('[data-storage-retention-paper-frame]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[64, 1161],
			[1374, 1161]
		]);
		const main = owner!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 1109]
		]);
		expect([...main.children[1].children].map(roundedSize)).toEqual([
			[240, 1109],
			[1134, 1109]
		]);
		const bands = [...owner!.querySelectorAll<HTMLElement>('[data-storage-band]')];
		expect(bands.map(roundedSize)).toEqual([
			[1070, 84],
			[1070, 128],
			[1070, 235],
			[1070, 216],
			[1070, 278]
		]);
		expect([...bands[2].children].map(roundedSize)).toEqual([
			[344, 235],
			[343, 235],
			[343, 235]
		]);
	});

	it('separates measured capacity and fixed pruning from unavailable history and policy', async () => {
		await page.viewport(1440, 1250);
		const { container } = await render(Board13StorageRetentionStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect.element(page.getByText('11 days', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('PROJECTED AT CONFIGURED CAP', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByText('Prune the oldest dated recordings', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('Stop recording when full', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Camera retention evidence', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByText(/Server update required · keeppeek\.offsite-archive\.v1/).first())
			.toBeVisible();
		expect(frame!.textContent).not.toContain('OLDEST FOOTAGE ON DISK');
		expect(frame!.textContent).not.toContain('smb://nas.lan');
		expect(frame!.textContent).not.toContain('Front Door');
		expect(frame!.textContent).not.toContain('3 OF 42');
	});
});
