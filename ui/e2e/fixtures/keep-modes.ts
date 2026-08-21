import type { Page } from '@playwright/test';
import type { CameraListItem } from '../../src/lib/types';
import {
	mockControlPeer,
	type ControlRequests,
	type MockControlPeerOptions,
	type StoredEventFixture,
	type StoredRangeFixture
} from './control-peer';

export const keepModeDate = '2026-08-18';
export const keepModeOlderDate = '2026-08-17';
export const keepModeDayStartMs = Date.parse(`${keepModeDate}T00:00:00Z`);
const jpeg = Buffer.from(
	'/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////2wBDAf//////////////////////////////////////////////////////////////////////////////////////wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAX/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIQAxAAAAEf/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPwF//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPyF//9oADAMBAAIAAwAAABD/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oACAEDAQE/EP/EABQRAQAAAAAAAAAAAAAAAAAAABD/2gAIAQIBAT8Q/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxB//9k=',
	'base64'
);

export function keepModeCameras(count = 10): CameraListItem[] {
	return Array.from({ length: count }, (_, index) => {
		const number = index + 1;
		const id = index === 0 ? 'front-door' : `camera-${number}`;
		return {
			id,
			ip: `192.0.2.${number}`,
			name: index === 0 ? 'Front Door' : `Camera ${number}`,
			manufacturer: 'ONVIF',
			model: `Model ${number}`,
			firmware_version: null,
			is_reolink: false,
			capabilities: {
				ptz: false,
				audio: false,
				events: true,
				recording: true,
				analytics: false,
				imaging: false,
				two_way_audio: false
			},
			profiles: [
				{
					name: 'Main',
					stream: 'main',
					encoding: 'h264',
					resolution: '640x360',
					framerate: 25,
					bitrate_kbps: 8_000
				}
			]
		};
	});
}

function recordings(cameraId: string, date: string) {
	const dateStartMs = Date.parse(`${date}T00:00:00Z`);
	const cameraOffsetMs =
		cameraId === 'front-door' ? 0 : Number(cameraId.split('-').at(-1)) * 40_000;
	return [
		{
			stream: 'main',
			date,
			hour: '06',
			filename: `${cameraId}-a.mp4`,
			url: `/keep-mode-${cameraId}-a.mp4`,
			start_time_ms: dateStartMs + 6 * 60 * 60_000 + cameraOffsetMs,
			end_time_ms: dateStartMs + 6 * 60 * 60_000 + 10 * 60_000 + cameraOffsetMs,
			duration_ms: 10 * 60_000
		},
		{
			stream: 'main',
			date,
			hour: '06',
			filename: `${cameraId}-b.mp4`,
			url: `/keep-mode-${cameraId}-b.mp4`,
			start_time_ms: dateStartMs + 6 * 60 * 60_000 + 15 * 60_000 + cameraOffsetMs,
			end_time_ms: dateStartMs + 6 * 60 * 60_000 + 35 * 60_000 + cameraOffsetMs,
			duration_ms: 20 * 60_000
		}
	];
}

function events(cameraId: string, date: string): StoredEventFixture[] {
	const dateStartMs = Date.parse(`${date}T00:00:00Z`);
	const cameraNumber = cameraId === 'front-door' ? 0 : Number(cameraId.split('-').at(-1));
	const fixtures: StoredEventFixture[] = [
		{
			sourceId: cameraId,
			event: {
				id: `${cameraId}-person`,
				source: 'camera',
				kind: 'person',
				start_time_ms: dateStartMs + 6 * 60 * 60_000 + 7 * 60_000 + cameraNumber * 40_000,
				end_time_ms: null,
				confidence: 0.88,
				bbox: null,
				zone: null,
				thumbnail_url: null
			}
		}
	];
	if (cameraId === 'front-door' && date === keepModeDate) {
		fixtures.push({
			sourceId: cameraId,
			thumbnail: jpeg,
			event: {
				id: 'story-1',
				source: 'camera',
				kind: 'story',
				start_time_ms: keepModeDayStartMs + 6 * 60 * 60_000 + 4 * 60_000,
				end_time_ms: keepModeDayStartMs + 6 * 60 * 60_000 + 6 * 60_000,
				confidence: null,
				bbox: null,
				zone: null,
				thumbnail_url: null
			}
		});
	}
	return fixtures;
}

export async function mockKeepModes(
	page: Page,
	cameraCount = 10,
	controlOptions: MockControlPeerOptions = {}
): Promise<ControlRequests> {
	const cameras = keepModeCameras(cameraCount);
	const storedRanges: StoredRangeFixture[] = cameras.flatMap((camera) =>
		[keepModeDate, keepModeOlderDate].flatMap((date) =>
			recordings(camera.id, date).map((recording) => ({
				sourceId: camera.id,
				streamId: recording.stream as 'main' | 'sub',
				startMs: recording.start_time_ms,
				endMs: recording.end_time_ms
			}))
		)
	);
	const storedEvents = cameras.flatMap((camera) =>
		[keepModeDate, keepModeOlderDate].flatMap((date) => events(camera.id, date))
	);
	const controls = await mockControlPeer(page, {
		...controlOptions,
		cameras,
		storedRanges,
		storedEvents,
		health: controlOptions.health ?? {
			status: 'healthy',
			cameras: cameras.map((camera) => ({
				id: camera.id,
				state: 'online',
				configured_profiles: camera.profiles
			}))
		}
	});
	await page.addInitScript(() => {
		Object.defineProperty(HTMLMediaElement.prototype, 'play', {
			configurable: true,
			value() {
				this.dataset.playRequested = 'true';
				return Promise.resolve();
			}
		});
	});
	return controls;
}
