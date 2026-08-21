import { describe, expect, it } from 'vitest';
import { eventSourceEvidence } from '$lib/event-sources';
import type { ServerHealthResponse } from '$lib/types';

describe('event-source evidence', () => {
	it('reports aggregate catalog facts without synthesizing registered publishers', () => {
		const health = {
			storage: {
				catalog: {
					events: 1_402,
					open_events: 2,
					event_thumbnails: 350
				}
			}
		} as ServerHealthResponse;

		expect(eventSourceEvidence(health)).toMatchObject({
			catalog: { totalEvents: 1_402, openEvents: 2, thumbnails: 350 },
			persistedOrigins: ['camera', 'keeppeek'],
			registeredPublishers: null,
			eventsToday: null,
			lastEventAtMs: null,
			tokenMetadata: null,
			permissions: null,
			typeMappings: null,
			publicationRuntime: 'unavailable'
		});
	});

	it('keeps catalog facts unavailable when health has no catalog snapshot', () => {
		expect(eventSourceEvidence(null).catalog).toBeNull();
		expect(
			eventSourceEvidence({ storage: { catalog: null } } as ServerHealthResponse).catalog
		).toBeNull();
	});

	it('identifies fields exposed by the stored-media WebRTC model', () => {
		const fields = eventSourceEvidence(null).storedMediaFields;

		expect(fields.transport).toBe('webrtc');
		expect(fields.exposed).toContain('source');
		expect(fields.exposed).toContain('thumbnail_url');
		expect(fields.notExposed).toEqual([
			'source_id',
			'revision',
			'text',
			'payload',
			'attachments[]'
		]);
	});
});
