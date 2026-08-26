import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { RecordingEvent, RecordingSegment } from '$lib/types';
import VerticalTimeline from './VerticalTimeline.svelte';

const DAY_MS = 24 * 60 * 60_000;

function pointer(type: string, pointerId: number, clientY: number): PointerEvent {
	return new PointerEvent(type, {
		bubbles: true,
		button: 0,
		cancelable: true,
		clientY,
		pointerId
	});
}

describe('VerticalTimeline', () => {
	it('scrolls with arrow keys without seeking playback', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const onSeek = vi.fn();
		await render(VerticalTimeline, {
			props: {
				segments: [],
				selectedUrl: null,
				playheadMs: null,
				dayStartMs,
				onSeek
			}
		});

		const timeline = page.getByRole('region', { name: /recording timeline scroll/i }).element();
		Object.defineProperties(timeline, {
			clientHeight: { configurable: true, value: 400 },
			scrollTop: { configurable: true, value: 0, writable: true }
		});
		const scrollControl = page
			.getByRole('button', { name: /scroll recording timeline/i })
			.element();
		scrollControl.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, key: 'ArrowDown' }));

		expect(timeline.scrollTop).toBe(72);
		expect(onSeek).not.toHaveBeenCalled();
		scrollControl.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, key: 'ArrowUp' }));
		expect(timeline.scrollTop).toBe(0);
	});

	it('renders an explicit gap between separated one-minute recordings', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const segments = [0, 1].map((index): RecordingSegment => ({
			stream: 'main',
			date: '2026-08-10',
			hour: '01',
			filename: `0${index}00.mp4`,
			url: `/recording-${index}.mp4`,
			start_time_ms: dayStartMs + (60 + index * 2) * 60_000,
			end_time_ms: dayStartMs + (61 + index * 2) * 60_000,
			duration_ms: 60_000
		}));
		const { container } = await render(VerticalTimeline, {
			props: {
				segments,
				selectedUrl: null,
				playheadMs: null,
				dayStartMs,
				onSeek: vi.fn()
			}
		});

		const gap = container.querySelector<HTMLElement>(
			`[data-timeline-gap][data-start-ms="${dayStartMs + 61 * 60_000}"]`
		);
		expect(gap).not.toBeNull();
		expect(Number.parseFloat(gap?.style.height ?? '0')).toBeGreaterThan(0);
	});

	it('seeks to the timestamp represented by an event thumbnail', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const eventStartMs = dayStartMs + 60 * 60_000;
		const onSeek = vi.fn();
		await render(VerticalTimeline, {
			props: {
				segments: [],
				events: [
					{
						id: 'event-1',
						source: 'camera',
						kind: 'motion',
						start_time_ms: eventStartMs,
						end_time_ms: eventStartMs + 10_000,
						confidence: null,
						bbox: null,
						zone: null,
						thumbnail_url: 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw=='
					}
				],
				selectedUrl: null,
				playheadMs: null,
				dayStartMs,
				onSeek
			}
		});

		await userEvent.click(page.getByRole('button', { name: /motion event at 01:00/i }));

		expect(onSeek).toHaveBeenCalledOnce();
		expect(onSeek).toHaveBeenCalledWith(eventStartMs);
	});

	it('seeks once when the playhead is dragged vertically', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const onSeek = vi.fn();
		await render(VerticalTimeline, {
			props: {
				segments: [],
				selectedUrl: null,
				playheadMs: dayStartMs + 12 * 60 * 60_000,
				dayStartMs,
				onSeek
			}
		});

		const scroller = page.getByRole('region', { name: /recording timeline scroll/i }).element();
		vi.spyOn(scroller, 'getBoundingClientRect').mockReturnValue({
			bottom: 2_688,
			height: 2_688,
			left: 0,
			right: 200,
			top: 0,
			width: 200,
			x: 0,
			y: 0,
			toJSON: () => ({})
		});
		const playhead = page.getByRole('button', { name: /playback position/i }).element();
		playhead.setPointerCapture = vi.fn();
		playhead.hasPointerCapture = vi.fn(() => true);
		playhead.releasePointerCapture = vi.fn();

		playhead.dispatchEvent(pointer('pointerdown', 7, 1_344));
		playhead.dispatchEvent(pointer('pointermove', 7, 672));
		expect(onSeek).not.toHaveBeenCalled();
		playhead.dispatchEvent(pointer('pointerup', 7, 672));

		expect(onSeek).toHaveBeenCalledOnce();
		expect(onSeek).toHaveBeenCalledWith(dayStartMs + 18 * 60 * 60_000);
	});

	it('reports scrub movement without committing the fallback seek callback', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const onSeek = vi.fn();
		const onScrubStart = vi.fn();
		const onScrub = vi.fn();
		const onScrubEnd = vi.fn();
		await render(VerticalTimeline, {
			props: {
				segments: [],
				selectedUrl: null,
				playheadMs: dayStartMs + 12 * 60 * 60_000,
				dayStartMs,
				onSeek,
				onScrubStart,
				onScrub,
				onScrubEnd
			}
		});

		const scroller = page.getByRole('region', { name: /recording timeline scroll/i }).element();
		vi.spyOn(scroller, 'getBoundingClientRect').mockReturnValue({
			bottom: 2_688,
			height: 2_688,
			left: 0,
			right: 200,
			top: 0,
			width: 200,
			x: 0,
			y: 0,
			toJSON: () => ({})
		});
		const playhead = page.getByRole('button', { name: /playback position/i }).element();
		playhead.setPointerCapture = vi.fn();
		playhead.hasPointerCapture = vi.fn(() => true);
		playhead.releasePointerCapture = vi.fn();

		playhead.dispatchEvent(pointer('pointerdown', 11, 1_344));
		playhead.dispatchEvent(pointer('pointermove', 11, 672));
		playhead.dispatchEvent(pointer('pointerup', 11, 672));

		expect(onScrubStart).toHaveBeenCalledWith(dayStartMs + 12 * 60 * 60_000);
		expect(onScrub).toHaveBeenCalledWith(dayStartMs + 18 * 60 * 60_000);
		expect(onScrubEnd).toHaveBeenCalledWith(dayStartMs + 18 * 60 * 60_000);
		expect(onSeek).not.toHaveBeenCalled();
	});

	it('stops following on manual scroll and returns to the newest edge', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const initialNowMs = dayStartMs + 12 * 60 * 60_000;
		const onSeek = vi.fn();
		const view = await render(VerticalTimeline, {
			props: {
				segments: [],
				selectedUrl: null,
				playheadMs: dayStartMs + 12 * 60 * 60_000,
				dayStartMs,
				nowMs: initialNowMs,
				onSeek
			}
		});

		const scroller = page.getByRole('region', { name: /recording timeline scroll/i }).element();
		scroller.dispatchEvent(new WheelEvent('wheel', { bubbles: true, deltaY: 100 }));
		await view.rerender({
			segments: [],
			selectedUrl: null,
			playheadMs: dayStartMs + 12 * 60 * 60_000,
			dayStartMs,
			nowMs: initialNowMs + 60 * 60_000,
			onSeek
		});
		expect(
			page.getByRole('region', { name: 'Recording timeline', exact: true }).element().dataset
				.timelineEndMs
		).toBe(String(initialNowMs));
		await userEvent.click(page.getByRole('button', { name: 'Back to live' }));

		expect(onSeek).toHaveBeenCalledWith(initialNowMs + 60 * 60_000);
	});

	it('steps through the five fixed Paper zoom levels', async () => {
		const { container } = await render(VerticalTimeline, {
			props: {
				segments: [],
				selectedUrl: null,
				playheadMs: null,
				dayStartMs: Date.UTC(2026, 7, 10),
				onSeek: vi.fn()
			}
		});
		const timeline = container.querySelector<HTMLElement>('[data-timeline-zoom]');
		const zoomIn = page.getByTitle('Zoom timeline in');
		const scroller = page.getByRole('region', { name: /recording timeline scroll/i }).element();
		Object.defineProperty(scroller, 'clientHeight', { configurable: true, value: 400 });
		scroller.dispatchEvent(new Event('scroll'));

		expect(timeline?.dataset.timelineZoom).toBe('6h');
		await userEvent.click(zoomIn);
		await userEvent.click(zoomIn);
		await userEvent.click(zoomIn);
		expect(timeline?.dataset.timelineZoom).toBe('1m');
		expect(container.querySelectorAll('[data-timeline-tick]').length).toBeLessThan(200);
	});

	it('bounds the initial render for a dense full-day history before viewport measurement', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const segments = Array.from({ length: 24 * 60 }, (_, index): RecordingSegment => {
			const startTimeMs = dayStartMs + index * 60_000;
			return {
				stream: 'main',
				date: '2026-08-10',
				hour: Math.floor(index / 60)
					.toString()
					.padStart(2, '0'),
				filename: `${index}.mp4`,
				url: `/recording-${index}.mp4`,
				start_time_ms: startTimeMs,
				end_time_ms: startTimeMs + 60_000,
				duration_ms: 60_000
			};
		});
		const events = Array.from({ length: 600 }, (_, index): RecordingEvent => {
			const startTimeMs = dayStartMs + Math.floor((index * DAY_MS) / 600);
			return {
				id: `event-${index}`,
				source: 'camera',
				kind: index % 2 === 0 ? 'person' : 'motion',
				start_time_ms: startTimeMs,
				end_time_ms: startTimeMs + 10_000,
				confidence: 0.9,
				bbox: null,
				zone: null,
				thumbnail_url: null
			};
		});
		const clientHeight = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(0);

		try {
			const view = await render(VerticalTimeline, {
				props: {
					segments,
					events,
					selectedUrl: null,
					playheadMs: dayStartMs + DAY_MS,
					dayStartMs,
					nowMs: dayStartMs + DAY_MS,
					onSeek: vi.fn()
				}
			});
			const renderedNodes = view.container.querySelectorAll(
				'[data-timeline-tick], [data-timeline-availability], [data-timeline-gap], [data-timeline-activity], [data-timeline-event-marker], [data-timeline-event]'
			).length;
			const offscreenEvents = Array.from({ length: 1_200 }, (_, index): RecordingEvent => {
				const startTimeMs = dayStartMs + Math.floor((index * 12 * 60 * 60_000) / 1_200);
				return {
					id: `offscreen-event-${index}`,
					source: 'camera',
					kind: 'motion',
					start_time_ms: startTimeMs,
					end_time_ms: startTimeMs + 10_000,
					confidence: null,
					bbox: null,
					zone: null,
					thumbnail_url: null
				};
			});

			await view.rerender({
				segments,
				events: [...events, ...offscreenEvents],
				selectedUrl: null,
				playheadMs: dayStartMs + DAY_MS,
				dayStartMs,
				nowMs: dayStartMs + DAY_MS,
				onSeek: vi.fn()
			});
			const nodesWithOffscreenHistory = view.container.querySelectorAll(
				'[data-timeline-tick], [data-timeline-availability], [data-timeline-gap], [data-timeline-activity], [data-timeline-event-marker], [data-timeline-event]'
			).length;

			expect(renderedNodes).toBeLessThan(800);
			expect(nodesWithOffscreenHistory).toBe(renderedNodes);
		} finally {
			clientHeight.mockRestore();
		}
	});

	it('reports a zoom-aligned viewport instead of requesting the full day', async () => {
		const onViewportChange = vi.fn();
		const dayStartMs = Date.UTC(2026, 7, 10);
		await render(VerticalTimeline, {
			props: {
				segments: [],
				selectedUrl: null,
				playheadMs: null,
				dayStartMs,
				onSeek: vi.fn(),
				onViewportChange
			}
		});
		const scroller = page.getByRole('region', { name: /recording timeline scroll/i }).element();
		Object.defineProperty(scroller, 'clientHeight', { configurable: true, value: 400 });
		scroller.dispatchEvent(new Event('scroll'));

		await vi.waitFor(() => expect(onViewportChange).toHaveBeenCalled());
		const viewport = onViewportChange.mock.lastCall?.[0];
		expect(viewport).toMatchObject({
			bucketMs: 5 * 60_000,
			prefetchMs: 60 * 60_000,
			eventTypes: []
		});
		expect(viewport.endMs - viewport.startMs).toBeLessThan(86_400_000);
	});

	it('filters event cards without dropping the underlying timeline', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const { container } = await render(VerticalTimeline, {
			props: {
				segments: [],
				events: [
					{
						id: 'person',
						source: 'camera',
						kind: 'person',
						start_time_ms: dayStartMs + 60_000,
						end_time_ms: null,
						confidence: null,
						bbox: null,
						zone: null,
						thumbnail_url: null
					},
					{
						id: 'motion',
						source: 'camera',
						kind: 'motion',
						start_time_ms: dayStartMs + 2 * 60_000,
						end_time_ms: null,
						confidence: null,
						bbox: null,
						zone: null,
						thumbnail_url: null
					}
				],
				selectedUrl: null,
				playheadMs: null,
				dayStartMs,
				onSeek: vi.fn()
			}
		});

		await userEvent.click(page.getByRole('button', { name: 'Motion', exact: true }));

		expect(container.querySelector('[data-timeline-event="motion"]')).not.toBeNull();
		expect(container.querySelector('[data-timeline-event="person"]')).toBeNull();
	});
});
