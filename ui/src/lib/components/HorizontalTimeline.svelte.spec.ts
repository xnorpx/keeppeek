import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import HorizontalTimeline from './HorizontalTimeline.svelte';

function pointer(type: string, pointerId: number, clientX: number): PointerEvent {
	return new PointerEvent(type, {
		bubbles: true,
		button: 0,
		cancelable: true,
		clientX,
		pointerId
	});
}

describe('HorizontalTimeline', () => {
	it('reports a bounded horizontal viewport and keyboard seek', async () => {
		const onSeek = vi.fn();
		const onViewportChange = vi.fn();
		const view = await render(HorizontalTimeline, {
			props: {
				segments: [],
				selectedUrl: null,
				playheadMs: Date.UTC(2026, 7, 10, 12),
				dayStartMs: Date.UTC(2026, 7, 10),
				nowMs: Date.UTC(2026, 7, 11),
				onSeek,
				onViewportChange
			}
		});
		const scroller = page.getByRole('slider', { name: /recording timeline scrubber/i }).element();
		Object.defineProperty(scroller, 'clientWidth', { configurable: true, value: 400 });
		scroller.dispatchEvent(new Event('scroll'));
		await view.rerender({
			segments: [],
			selectedUrl: null,
			playheadMs: Date.UTC(2026, 7, 10, 12),
			dayStartMs: Date.UTC(2026, 7, 10),
			nowMs: Date.UTC(2026, 7, 11),
			onSeek,
			onViewportChange
		});

		await vi.waitFor(() => expect(onViewportChange).toHaveBeenCalled());
		expect(onViewportChange.mock.lastCall?.[0]).toMatchObject({
			bucketMs: 5 * 60_000,
			prefetchMs: 60 * 60_000,
			viewportExtentPx: 400
		});
		expect(document.querySelectorAll('[data-timeline-orientation="horizontal"]')).toHaveLength(1);
		await userEvent.type(
			page.getByRole('slider', { name: /recording timeline scrubber/i }),
			'{ArrowRight}'
		);
		expect(onSeek).toHaveBeenCalledOnce();
		expect(Number.isFinite(onSeek.mock.calls[0]?.[0])).toBe(true);
	});

	it('reports horizontal drag samples without invoking the fallback seek', async () => {
		const onSeek = vi.fn();
		const onScrubStart = vi.fn();
		const onScrub = vi.fn();
		const onScrubEnd = vi.fn();
		await render(HorizontalTimeline, {
			props: {
				segments: [],
				selectedUrl: null,
				playheadMs: Date.UTC(2026, 7, 10, 12),
				dayStartMs: Date.UTC(2026, 7, 10),
				nowMs: Date.UTC(2026, 7, 11),
				onSeek,
				onScrubStart,
				onScrub,
				onScrubEnd
			}
		});
		const scroller = page.getByRole('slider', { name: /recording timeline scrubber/i }).element();
		Object.defineProperty(scroller, 'clientWidth', { configurable: true, value: 400 });
		scroller.setPointerCapture = vi.fn();
		scroller.hasPointerCapture = vi.fn(() => true);
		scroller.releasePointerCapture = vi.fn();
		scroller.dispatchEvent(pointer('pointerdown', 17, 200));
		scroller.dispatchEvent(pointer('pointermove', 17, 100));
		scroller.dispatchEvent(pointer('pointerup', 17, 100));

		expect(onScrubStart).toHaveBeenCalledOnce();
		expect(onScrub).toHaveBeenCalledOnce();
		expect(onScrubEnd).toHaveBeenCalledOnce();
		expect(onSeek).not.toHaveBeenCalled();
	});
});
