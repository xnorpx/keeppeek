import type { ServerHealthResponse } from '$lib/types';

export type EventSourceEvidence = {
	catalog: {
		totalEvents: number;
		openEvents: number;
		thumbnails: number;
	} | null;
	persistedOrigins: readonly ['camera', 'keeppeek'];
	registeredPublishers: null;
	eventsToday: null;
	lastEventAtMs: null;
	tokenMetadata: null;
	permissions: null;
	typeMappings: null;
	publicationRuntime: 'unavailable';
	storedMediaFields: {
		transport: 'webrtc';
		exposed: readonly string[];
		notExposed: readonly string[];
	};
};

const exposedStoredMediaFields = Object.freeze([
	'id',
	'camera_id',
	'date',
	'source',
	'kind',
	'start_time_ms',
	'end_time_ms',
	'confidence',
	'bbox',
	'zone',
	'thumbnail_url'
]);

const unexposedStoredMediaFields = Object.freeze([
	'source_id',
	'revision',
	'text',
	'payload',
	'attachments[]'
]);

export function eventSourceEvidence(health: ServerHealthResponse | null): EventSourceEvidence {
	const catalog = health?.storage.catalog;
	return {
		catalog: catalog
			? {
					totalEvents: catalog.events,
					openEvents: catalog.open_events,
					thumbnails: catalog.event_thumbnails
				}
			: null,
		persistedOrigins: ['camera', 'keeppeek'],
		registeredPublishers: null,
		eventsToday: null,
		lastEventAtMs: null,
		tokenMetadata: null,
		permissions: null,
		typeMappings: null,
		publicationRuntime: 'unavailable',
		storedMediaFields: {
			transport: 'webrtc',
			exposed: exposedStoredMediaFields,
			notExposed: unexposedStoredMediaFields
		}
	};
}
