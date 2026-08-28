import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board10EventsStory from '../../../visual-harness/stories/Board10EventsStory.svelte';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 10 Events stories', () => {
	it('renders the browse shell, result lanes, and shared cards at exact Paper dimensions', async () => {
		await page.viewport(1440, 900);
		const { container } = await render(Board10EventsStory, { state: 'browse' });
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="events.desktop.browse"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 820]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[64, 818],
			[1374, 818]
		]);
		const main = frame!.querySelector<HTMLElement>('[data-events-browse-main]');
		expect(main).not.toBeNull();
		expect([...main!.children].map(roundedSize)).toEqual([
			[1374, 56],
			[1374, 40],
			[1374, 722]
		]);
		const rows = [...frame!.querySelectorAll<HTMLElement>('[data-events-row]')];
		expect(rows.map(roundedSize)).toEqual([
			[1342, 218],
			[1342, 216],
			[1342, 216]
		]);
		const cards = [...frame!.querySelectorAll<HTMLElement>('[data-event-paper-frame]')];
		expect(cards).toHaveLength(15);
		expect(frame!.querySelector('img')).toBeNull();
		expect(roundedSize(cards[0])).toEqual([260, 218]);
		expect(roundedSize(cards[1])).toEqual([258, 218]);
		expect(cards.map((card) => roundedSize(card.firstElementChild!)[1])).toEqual(
			Array.from({ length: 15 }, () => 132)
		);
	});

	it('renders the exact detail composition and reports unsupported evidence honestly', async () => {
		await page.viewport(1440, 800);
		const { container } = await render(Board10EventsStory, { state: 'detail' });
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="events.desktop.detail"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 669]);
		expect([...frame!.children].map(roundedSize)).toEqual([
			[560, 628],
			[852, 628]
		]);
		const drawer = frame!.querySelector<HTMLElement>('[data-event-detail]');
		expect(drawer).not.toBeNull();
		expect(drawer!.querySelector('img')).toBeNull();
		expect([...drawer!.children].map(roundedSize)).toEqual([
			[558, 47],
			[558, 250],
			[558, 65],
			[558, 264]
		]);

		await expect
			.element(page.getByText('ONE THUMBNAIL URL', { exact: true }).first())
			.toBeVisible();
		await expect
			.element(page.getByText('Not reported by REST API', { exact: false }).first())
			.toBeVisible();
		expect(frame!.textContent).toContain('Publisher identity is unavailable.');
		expect(frame!.textContent).not.toContain('REV 3');
		expect(frame!.textContent).not.toContain('dwell_seconds');
		expect(frame!.textContent).not.toContain('object-detect');
	});
});
