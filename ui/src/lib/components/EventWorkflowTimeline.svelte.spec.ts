import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { RecordingEvent } from '$lib/types';
import HorizontalTimeline from './HorizontalTimeline.svelte';
import VerticalTimeline from './VerticalTimeline.svelte';
import KeepStories from './KeepStories.svelte';

function events(dayStartMs: number): RecordingEvent[] {
	return [1, 2, 3].map((index) => ({
		id: `event-${index}`,
		source_id: 'source',
		source: 'camera',
		kind: 'motion',
		start_time_ms: dayStartMs + index * 1000,
		end_time_ms: null,
		confidence: null,
		bbox: null,
		zone: null,
		thumbnail_url: null,
		workflow: {
			sourceId: 'source',
			eventId: `event-${index}`,
			reviewed: false,
			dismissed: false,
			reviewRevision: '0',
			reviewedAtMs: null,
			dismissedAtMs: null,
			updatedAtMs: null,
			eventPresent: true,
			mediaAvailable: false,
			sourceAvailable: true,
			bookmark:
				index === 2
					? {
							active: true,
							note: '',
							revision: '1',
							createdBy: 'alice',
							createdAtMs: 1000,
							updatedBy: 'alice',
							updatedAtMs: 1000,
							eventStartMs: dayStartMs + 2000,
							eventKind: 'motion',
							audit: []
						}
					: null
		}
	}));
}

describe('workflow timeline and story markers', () => {
	for (const [name, Component] of [
		['horizontal', HorizontalTimeline],
		['vertical', VerticalTimeline]
	] as const) {
		it(`retains a bookmarked representative in a dense ${name} event cluster`, async () => {
			const dayStartMs = Date.UTC(2026, 7, 18);
			const onEventPreview = vi.fn();
			const view = await render(Component, {
				props: {
					segments: [],
					events: events(dayStartMs),
					selectedUrl: null,
					playheadMs: dayStartMs + 2000,
					dayStartMs,
					nowMs: dayStartMs + 60_000,
					onSeek: vi.fn(),
					onEventPreview
				}
			});
			const marker = view.container.querySelector<HTMLButtonElement>(
				'[data-timeline-bookmarked="true"]'
			);
			expect(marker).not.toBeNull();
			marker!.click();
			expect(onEventPreview.mock.calls[0]?.[0].id).toBe('event-2');
		});
	}

	it('shows a shared bookmark marker on Keep stories', async () => {
		const dayStartMs = Date.UTC(2026, 7, 18);
		const view = await render(KeepStories, {
			props: {
				events: [{ ...events(dayStartMs)[1]!, kind: 'story' }],
				dates: ['2026-08-18'],
				selectedDate: '2026-08-18',
				ondate: vi.fn(),
				onseek: vi.fn()
			}
		});
		expect(view.container.querySelector('[aria-label="Bookmarked story"]')).not.toBeNull();
	});
});
