import { describe, expect, it } from 'vitest';
import type { RecordingEvent, RecordingEventAttachment } from './types';
import {
	canonicalEventAttachment,
	eventIconKey,
	eventOwnsCanonicalBoundingBox,
	orderedEventAttachments
} from './event-presentation';

function attachment(
	id: string,
	type: string,
	contentType: string,
	ordinal: number,
	timestampMs: number | null
): RecordingEventAttachment {
	return {
		id,
		type,
		content_type: contentType,
		byte_length: 12,
		ordinal,
		timestamp_ms: timestampMs
	};
}

function event(overrides: Partial<RecordingEvent> = {}): RecordingEvent {
	return {
		id: 'event-1',
		source: 'camera',
		kind: 'person',
		start_time_ms: 1_000,
		end_time_ms: null,
		confidence: null,
		bbox: [0.1, 0.2, 0.3, 0.4],
		zone: null,
		thumbnail_url: null,
		...overrides
	};
}

describe('canonical event presentation', () => {
	it('selects supported images by type, ordinal, timestamp, and stable ID', () => {
		const attachments = [
			attachment('story', 'story-frame', 'image/jpeg', 0, 1),
			attachment('late', 'snapshot', 'image/jpeg', 0, 20),
			attachment('z-stable', 'snapshot', 'image/jpeg', 0, 10),
			attachment('a-stable', 'snapshot', 'image/jpeg', 0, 10),
			attachment('first-ordinal', 'snapshot', 'image/jpeg', 1, 0)
		];

		expect(canonicalEventAttachment(attachments)?.id).toBe('a-stable');
	});

	it('honors only a unique supported explicit reference', () => {
		const attachments = [
			attachment('snapshot', 'snapshot', 'image/jpeg', 0, 10),
			attachment('story', 'story-frame', 'image/webp', 3, 30)
		];

		expect(canonicalEventAttachment(attachments, 'story')?.id).toBe('story');
		expect(canonicalEventAttachment(attachments, 'missing')).toBeNull();
		expect(
			canonicalEventAttachment([attachment('text', 'snapshot', 'text/plain', 0, null)], 'text')
		).toBeNull();
		expect(
			canonicalEventAttachment([
				attachment('duplicate', 'snapshot', 'image/jpeg', 0, 1),
				attachment('duplicate', 'snapshot', 'image/jpeg', 1, 2)
			])
		).toBeNull();
	});

	it('uses only allowlisted icon keys and deterministic event-type fallbacks', () => {
		expect(eventIconKey('vehicle', 'person')).toBe('vehicle');
		expect(eventIconKey(undefined, 'FACE')).toBe('person');
		for (const key of [
			'<svg onload=alert(1)>',
			'javascript:alert(1)',
			'https://example.com/icon.svg',
			'class-name text-red-500',
			'x'.repeat(256)
		]) {
			expect(eventIconKey(key, 'story')).toBe('story');
		}
	});

	it('draws a bounding box only in the canonical attachment coordinate space', () => {
		expect(
			eventOwnsCanonicalBoundingBox(
				event({ canonical_attachment_id: 'snapshot-1', bbox_attachment_id: 'snapshot-1' })
			)
		).toBe(true);
		expect(
			eventOwnsCanonicalBoundingBox(
				event({ canonical_attachment_id: 'snapshot-1', bbox_attachment_id: 'story-1' })
			)
		).toBe(false);
		expect(
			eventOwnsCanonicalBoundingBox(
				event({
					bbox: null,
					canonical_attachment_id: 'snapshot-1',
					bbox_attachment_id: 'snapshot-1'
				})
			)
		).toBe(false);
	});

	it('places the canonical frame first without losing deterministic story order', () => {
		const attachments = [
			attachment('frame-2', 'story-frame', 'image/jpeg', 2, 30),
			attachment('frame-0', 'story-frame', 'image/jpeg', 0, 10),
			attachment('frame-1', 'story-frame', 'image/jpeg', 1, 20)
		];
		expect(
			orderedEventAttachments(event({ attachments, canonical_attachment_id: 'frame-2' })).map(
				(candidate) => candidate.id
			)
		).toEqual(['frame-2', 'frame-0', 'frame-1']);
	});
});
