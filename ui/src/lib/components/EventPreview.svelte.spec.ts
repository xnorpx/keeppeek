import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { RecordingEvent } from '$lib/types';
import EventPreview from './EventPreview.svelte';

const canonicalAttachment = {
	id: 'snapshot-1',
	type: 'snapshot',
	content_type: 'image/jpeg',
	byte_length: 12,
	ordinal: 0,
	timestamp_ms: 1_000
};

function event(overrides: Partial<RecordingEvent> = {}): RecordingEvent {
	return {
		id: 'event-1',
		source_id: 'front-door',
		revision: 2,
		source: 'camera',
		kind: 'person',
		start_time_ms: 1_000,
		end_time_ms: null,
		confidence: 0.9,
		bbox: [0.1, 0.2, 0.3, 0.4],
		bbox_attachment_id: 'story-1',
		zone: null,
		thumbnail_url: 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==',
		attachments: [canonicalAttachment],
		canonical_attachment_id: canonicalAttachment.id,
		icon_key: 'person',
		image_availability: 'available',
		...overrides
	};
}

describe('EventPreview', () => {
	it('renders a bounding box only for the canonical attachment coordinate space', async () => {
		const view = await render(EventPreview, {
			props: { event: event(), cameraLabel: 'Front door', showBoundingBox: true }
		});
		expect(view.container.querySelector('[data-event-preview-image]')).not.toBeNull();
		expect(view.container.querySelector('[data-event-bounding-box]')).toBeNull();

		await view.rerender({
			event: event({ bbox_attachment_id: canonicalAttachment.id }),
			cameraLabel: 'Front door',
			showBoundingBox: true
		});
		expect(view.container.querySelector('[data-event-bounding-box]')).not.toBeNull();
	});

	it('shows one explicit unavailable state and retries the same canonical image', async () => {
		const onretry = vi.fn();
		const unavailable = event({
			thumbnail_url: null,
			image_availability: 'unavailable'
		});
		const view = await render(EventPreview, {
			props: {
				event: unavailable,
				cameraLabel: 'Front door',
				previewState: 'unavailable',
				onretry
			}
		});

		expect(view.container.querySelector('[data-event-preview-state="unavailable"]')).not.toBeNull();
		expect(view.container.textContent).toContain('PREVIEW UNAVAILABLE');
		await userEvent.click(page.getByRole('button', { name: 'Retry preview' }));
		expect(onretry).toHaveBeenCalledOnce();
	});
});
