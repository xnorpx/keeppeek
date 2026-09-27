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
	it('centers the latest playhead when playback changes before the first frame', async () => {
		const frames = new Map<number, FrameRequestCallback>();
		let frameId = 0;
		const schedule = vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
			frames.set(++frameId, callback);
			return frameId;
		});
		const cancel = vi.spyOn(window, 'cancelAnimationFrame').mockImplementation((id) => {
			frames.delete(id);
		});
		try {
			const dayStartMs = Date.UTC(2026, 7, 10);
			const view = await render(HorizontalTimeline, {
				props: {
					segments: [],
					selectedUrl: null,
					playheadMs: dayStartMs + 12 * 3_600_000,
					dayStartMs,
					nowMs: dayStartMs + 86_400_000,
					onSeek: vi.fn()
				}
			});
			const scroller = page.getByRole('slider', { name: /recording timeline scrubber/i }).element();
			Object.assign(scroller.style, { width: '400px', overflowX: 'auto' });
			scroller.dispatchEvent(new Event('scroll'));
			expect(frames.size).toBeGreaterThan(0);
			const latestPlayheadMs = dayStartMs + 16 * 3_600_000;
			await view.rerender({ playheadMs: latestPlayheadMs });
			const pending = [...frames.values()];
			frames.clear();
			for (const callback of pending) callback(performance.now());
			expect(scroller.scrollLeft).toBeGreaterThan(0);
			await expect.element(scroller).toHaveAttribute('aria-valuetext', '16:00 UTC');
			scroller.scrollLeft = 0;
			scroller.dispatchEvent(new Event('scroll'));
			await view.rerender({ playheadMs: latestPlayheadMs + 1_000 });
			expect(frames.size).toBe(0);
			expect(scroller.scrollLeft).toBe(0);
			await view.rerender({
				dayStartMs: dayStartMs + 86_400_000,
				playheadMs: latestPlayheadMs + 86_400_000
			});
			expect(frames.size).toBe(1);
			await view.unmount();
			expect(frames.size).toBe(0);
		} finally {
			schedule.mockRestore();
			cancel.mockRestore();
		}
	});

	it.each([-12, 36])('waits for the selected day playhead instead of hour %i', async (hour) => {
		const frames = new Map<number, FrameRequestCallback>();
		let frameId = 0;
		const schedule = vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
			frames.set(++frameId, callback);
			return frameId;
		});
		const cancel = vi.spyOn(window, 'cancelAnimationFrame').mockImplementation((id) => {
			frames.delete(id);
		});
		try {
			const dayStartMs = Date.UTC(2026, 7, 10);
			const view = await render(HorizontalTimeline, {
				props: {
					segments: [],
					selectedUrl: null,
					playheadMs: dayStartMs + hour * 3_600_000,
					dayStartMs,
					nowMs: dayStartMs + 86_400_000,
					onSeek: vi.fn()
				}
			});
			const scroller = page.getByRole('slider', { name: /recording timeline scrubber/i }).element();
			Object.assign(scroller.style, { width: '400px', overflowX: 'auto' });
			scroller.dispatchEvent(new Event('scroll'));
			await view.rerender({});
			const staleFrames = [...frames.values()];
			frames.clear();
			for (const callback of staleFrames) callback(performance.now());
			await view.rerender({ playheadMs: dayStartMs + 12 * 3_600_000 });
			const pending = [...frames.values()];
			frames.clear();
			for (const callback of pending) callback(performance.now());
			expect(scroller.scrollLeft).toBeGreaterThan(0);
			await expect.element(scroller).toHaveAttribute('aria-valuetext', '12:00 UTC');
			await view.unmount();
		} finally {
			schedule.mockRestore();
			cancel.mockRestore();
		}
	});

	it('renders an open operational interval that began before the viewport', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		await render(HorizontalTimeline, {
			props: {
				segments: [],
				events: [
					{
						id: 'outage-1',
						source: 'keeppeek',
						kind: 'camera_offline',
						start_time_ms: dayStartMs,
						end_time_ms: null,
						confidence: null,
						bbox: null,
						zone: null,
						thumbnail_url: null,
						operational: {
							kind: 'camera_offline',
							severity: 'critical',
							cause: 'transport_disconnected',
							explanation: 'Camera transport is disconnected',
							affected_streams: ['main', 'sub'],
							recording_interrupted: true,
							evidence_source: 'canonical_health',
							stream_id: null,
							duration_ms: null,
							recovered: false
						}
					}
				],
				selectedUrl: null,
				playheadMs: dayStartMs + 60_000,
				dayStartMs,
				nowMs: dayStartMs + 120_000,
				onSeek: vi.fn()
			}
		});

		expect(document.querySelector('[data-timeline-operational-event="outage-1"]')).not.toBeNull();
	});

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
