import { describe, expect, it } from 'vitest';
import { thumbnailEvictionKeys, timelineThumbnailDiskKey } from './timeline-thumbnail-disk-cache';

describe('TimelineThumbnailDiskCache', () => {
	it('keys entries by source, revision, attachment, and size class', () => {
		expect(
			timelineThumbnailDiskKey({
				sourceId: 'front/door',
				eventId: 'event:42',
				revision: 3,
				attachmentId: 'thumbnail',
				sizeClass: 320
			})
		).toBe('front%2Fdoor:event%3A42:3:thumbnail:320');
	});

	it('evicts least-recently-used entries until the byte budget is met', () => {
		expect(
			thumbnailEvictionKeys(
				[
					{ key: 'newest', byteLength: 40, lastAccessMs: 30 },
					{ key: 'oldest', byteLength: 30, lastAccessMs: 10 },
					{ key: 'middle', byteLength: 40, lastAccessMs: 20 }
				],
				60
			)
		).toEqual(['oldest', 'middle']);
	});
});
