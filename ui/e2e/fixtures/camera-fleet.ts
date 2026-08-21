import type { Page } from '@playwright/test';
import type { CameraListItem } from '../../src/lib/types';
import { mockControlPeer, type HealthFixture } from './control-peer';

const namedCameras = [
	{ id: 'front-door', name: 'Front Door' },
	{ id: 'porch', name: 'Porch' },
	{ id: 'back-yard', name: 'Back Yard' }
] as const;

export function cameraFleet(count = 127): CameraListItem[] {
	return Array.from({ length: count }, (_, index) => {
		const number = index + 1;
		const named = namedCameras[index];
		return {
			id: named?.id ?? `camera-${String(number).padStart(3, '0')}`,
			ip: `192.0.2.${number}`,
			name: named?.name ?? `Camera ${String(number).padStart(3, '0')}`,
			manufacturer: index % 2 === 0 ? 'Reolink' : 'ONVIF',
			model: index % 2 === 0 ? 'RLC-811A' : 'DS-2CD2143',
			firmware_version: null,
			is_reolink: index % 2 === 0,
			backend: index % 2 === 0 ? 'Reolink' : 'ONVIF',
			transport: index % 2 === 0 ? 'Baichuan · TCP' : 'RTSP · TCP',
			capabilities: {
				ptz: index % 9 === 0,
				audio: true,
				events: true,
				recording: true,
				analytics: false,
				imaging: true,
				two_way_audio: false
			},
			profiles: [
				{
					name: 'Main',
					stream: 'main',
					encoding: 'h265',
					resolution: '3840x2160',
					framerate: 25
				},
				{
					name: 'Sub',
					stream: 'sub',
					encoding: 'h264',
					resolution: '640x360',
					framerate: 15
				}
			]
		};
	});
}

export function cameraFleetHealth(count = 127): NonNullable<HealthFixture['cameras']> {
	return cameraFleet(count).map((camera, index) => {
		const state = index === 1 ? 'degraded' : index === 2 ? 'offline' : 'online';
		const drops = index === 1 ? 14 : 0;
		return {
			id: camera.id,
			ip: camera.ip,
			name: camera.name ?? camera.id,
			manufacturer: camera.manufacturer,
			model: camera.model,
			firmware_version: null,
			backend: camera.backend,
			transport: camera.transport,
			state,
			lifecycle: state === 'offline' ? 'Stopped' : 'Connected',
			last_error: state === 'offline' ? 'Authentication failed' : null,
			configured_profiles: camera.profiles,
			streams: [
				{
					type: 'main',
					codec: 'h265',
					resolution: '3840x2160',
					fps: state === 'offline' ? 0 : 25,
					kbps: state === 'offline' ? undefined : 6_200,
					frames: state === 'offline' ? 0 : 86,
					drops,
					updated_at_ms: 1,
					report_age_ms: state === 'offline' ? 8_040_000 : 20
				}
			]
		};
	});
}

export async function mockCameraFleet(
	page: Page,
	count = 127,
	options: { healthGate?: Promise<void> } = {}
): Promise<void> {
	await mockControlPeer(page, {
		cameras: cameraFleet(count),
		healthGate: options.healthGate,
		health: {
			status: 'degraded',
			generated_at_ms: 1,
			uptime_seconds: 1,
			version: 'test',
			cameras: cameraFleetHealth(count),
			issues: []
		}
	});
}
