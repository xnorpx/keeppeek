import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board19GroupsStory from '../../../visual-harness/stories/Board19GroupsStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

async function renderState(state: 'administration' | 'participant', height: number) {
	await page.viewport(1440, 900);
	const { container } = await render(Board19GroupsStory, { props: { state } });
	await document.fonts.ready;
	const frame = container.querySelector<HTMLElement>(
		`[data-paper-scenario="groups.desktop.${state}"]`
	);
	expect(frame).not.toBeNull();
	expect(roundedSize(frame!)).toEqual([1440, height]);
	return frame!;
}

describe('Board 19 Groups stories', () => {
	it('renders the administration shell in exact Paper geometry without a fictional directory', async () => {
		const frame = await renderState('administration', 416);
		const owner = frame.querySelector<HTMLElement>('[data-groups-paper-frame="administration"]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[64, 414],
			[1374, 414]
		]);
		const rows = [...owner!.querySelectorAll<HTMLElement>('[data-group-evidence-row]')];
		expect(rows.map(roundedSize)).toEqual([
			[1310, 56],
			[1310, 56],
			[1310, 56]
		]);
		await expect
			.element(page.getByText('Group directory unavailable', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByText(/Server update required · keeppeek\.group-admin\.v1/))
			.toBeVisible();
		expect(frame.textContent).not.toContain('Front of house');
		expect(frame.textContent).not.toContain('Yard & perimeter');
		expect(frame.textContent).not.toContain('Shop floor intercom');
	});

	it('renders the participant contract in exact geometry without inventing joined people', async () => {
		const frame = await renderState('participant', 420);
		const owner = frame.querySelector<HTMLElement>('[data-groups-paper-frame="participant"]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[940, 270],
			[480, 420]
		]);
		const cards = [...owner!.querySelectorAll<HTMLElement>('[data-participant-evidence-card]')];
		expect(cards.map(roundedSize)).toEqual([
			[132, 120],
			[132, 120],
			[132, 120],
			[132, 120]
		]);
		await expect
			.element(page.getByText('Participant state unavailable', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('No floor control', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Join required for local talk control' }))
			.toBeDisabled();
		expect(frame.textContent).not.toContain('Marcus');
		expect(frame.textContent).not.toContain('Anna');
		expect(frame.textContent).not.toContain('2 talking');
	});
});
