import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { RecordingSegment } from '$lib/types';
import VerticalTimeline from './VerticalTimeline.svelte';

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
	it('seeks one minute forward from the start of the selected day', async () => {
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

		const timeline = page.getByRole('button', { name: /seek recording timeline/i });
		await userEvent.type(timeline, '{ArrowDown}');

		expect(onSeek).toHaveBeenCalledOnce();
		expect(onSeek).toHaveBeenCalledWith(dayStartMs + 60_000);
	});

	it('leaves a visible gap between adjacent one-minute recordings', async () => {
		const dayStartMs = Date.UTC(2026, 7, 10);
		const segments = [0, 1].map((index): RecordingSegment => ({
			stream: 'main',
			date: '2026-08-10',
			hour: '01',
			filename: `0${index}00.mp4`,
			url: `/recording-${index}.mp4`,
			start_time_ms: dayStartMs + (60 + index) * 60_000,
			end_time_ms: dayStartMs + (61 + index) * 60_000,
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

		const clips = container.querySelectorAll<HTMLButtonElement>('button[title*="–"]');
		expect(clips).toHaveLength(2);
		const firstBottom =
			Number.parseFloat(clips[0].style.top) + Number.parseFloat(clips[0].style.height);
		const secondTop = Number.parseFloat(clips[1].style.top);

		expect(firstBottom).toBeLessThan(secondTop);
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

		const scroller = page.getByRole('region', { name: /recording timeline pan/i }).element();
		vi.spyOn(scroller, 'getBoundingClientRect').mockReturnValue({
			bottom: 1_728,
			height: 1_728,
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

		playhead.dispatchEvent(pointer('pointerdown', 7, 864));
		playhead.dispatchEvent(pointer('pointermove', 7, 432));
		expect(onSeek).not.toHaveBeenCalled();
		playhead.dispatchEvent(pointer('pointerup', 7, 432));

		expect(onSeek).toHaveBeenCalledOnce();
		expect(onSeek).toHaveBeenCalledWith(dayStartMs + 6 * 60 * 60_000);
	});
});
