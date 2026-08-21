import type { Page } from '@playwright/test';
import type { RecordingEvent } from '../../src/lib/types';
import { mockControlPeer, type StoredEventFixture, type StoredRangeFixture } from './control-peer';

export const eventDate = '2026-08-18';
const dayStartMs = Date.parse(`${eventDate}T00:00:00Z`);
const personHighTimestampMs = dayStartMs + 6 * 60 * 60_000 + 37 * 60_000 + 23_000;
const jpeg = Buffer.from(
	'/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////2wBDAf//////////////////////////////////////////////////////////////////////////////////////wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAX/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIQAxAAAAEf/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPwF//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxB//9k=',
	'base64'
);

const cameras = [
	{
		id: 'front-door',
		ip: '192.0.2.1',
		name: 'Front Door',
		manufacturer: 'Reolink',
		model: 'RLC-811A',
		firmware_version: null,
		is_reolink: true,
		profiles: []
	},
	{
		id: 'driveway',
		ip: '192.0.2.2',
		name: 'Driveway',
		manufacturer: 'ONVIF',
		model: 'DS-2CD2143',
		firmware_version: null,
		is_reolink: false,
		profiles: []
	}
];

const frontDoorEvents: RecordingEvent[] = [
	{
		id: 'person-high',
		source: 'camera',
		kind: 'person',
		start_time_ms: personHighTimestampMs,
		end_time_ms: personHighTimestampMs + 5_000,
		confidence: 0.94,
		bbox: [0.3, 0.2, 0.25, 0.5],
		zone: 'porch',
		thumbnail_url: '/events-person-high.jpg'
	},
	{
		id: 'story-1',
		source: 'keeppeek',
		kind: 'story',
		start_time_ms: dayStartMs + 6 * 60 * 60_000 + 12 * 60_000,
		end_time_ms: dayStartMs + 6 * 60 * 60_000 + 14 * 60_000,
		confidence: null,
		bbox: null,
		zone: null,
		thumbnail_url: '/events-story.jpg'
	},
	{
		id: 'motion-no-image',
		source: 'camera',
		kind: 'motion',
		start_time_ms: dayStartMs + 5 * 60 * 60_000 + 48 * 60_000,
		end_time_ms: null,
		confidence: null,
		bbox: null,
		zone: null,
		thumbnail_url: null
	},
	{
		id: 'person-low',
		source: 'camera',
		kind: 'person',
		start_time_ms: dayStartMs + 5 * 60 * 60_000 + 21 * 60_000,
		end_time_ms: null,
		confidence: 0.42,
		bbox: null,
		zone: 'walkway',
		thumbnail_url: '/events-person-low.jpg'
	}
];

const drivewayEvents: RecordingEvent[] = [
	{
		id: 'vehicle-1',
		source: 'camera',
		kind: 'vehicle',
		start_time_ms: dayStartMs + 6 * 60 * 60_000 + 4 * 60_000,
		end_time_ms: null,
		confidence: 0.88,
		bbox: [0.1, 0.4, 0.6, 0.3],
		zone: 'driveway',
		thumbnail_url: '/events-vehicle.jpg'
	}
];

export async function mockEvents(page: Page): Promise<void> {
	const storedRanges: StoredRangeFixture[] = [
		{
			sourceId: 'front-door',
			streamId: 'main',
			startMs: personHighTimestampMs,
			endMs: personHighTimestampMs + 1_000
		}
	];
	const storedEvents: StoredEventFixture[] = [
		...frontDoorEvents.map((event) => ({
			sourceId: 'front-door',
			event,
			thumbnail: event.thumbnail_url === null ? undefined : jpeg
		})),
		...drivewayEvents.map((event) => ({
			sourceId: 'driveway',
			event,
			thumbnail: event.thumbnail_url === null ? undefined : jpeg
		}))
	];
	await mockControlPeer(page, { cameras, storedRanges, storedEvents });
}
