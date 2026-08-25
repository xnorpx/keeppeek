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
		const state = index === 1 ? 'degraded' : index === 2 ? 'offline' : 'healthy';
		const drops = index === 1 ? 14 : 0;
		const current = state !== 'offline';
		const reason =
			state === 'healthy'
				? 'healthy'
				: state === 'degraded'
					? 'ingress_drops'
					: 'transport_disconnected';
		const detail =
			state === 'healthy'
				? 'Transport, media, keyframe, and recording evidence is current'
				: state === 'degraded'
					? '14% frames dropped'
					: 'Authentication failed';
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
			reason,
			reason_codes: [reason],
			detail,
			dimensions: {
				configured: true,
				expected: true,
				transport_connected: current,
				frames_fresh: current,
				decodable: current,
				recording_requested: true,
				recording_progressing: current,
				configured_video_streams: 2,
				connected_video_streams: current ? 2 : 0,
				reporting_video_streams: 1,
				fresh_video_streams: current ? 1 : 0,
				decodable_video_streams: current ? 1 : 0,
				recording_video_streams: 1,
				recording_streams_progressing: current ? 1 : 0
			},
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
					report_age_ms: state === 'offline' ? 8_040_000 : 20,
					state,
					reason,
					reason_codes: [reason],
					detail,
					dimensions: {
						expected: true,
						transport_connected: current,
						report_fresh: current,
						frames_fresh: current,
						decodable: current,
						recording_requested: true,
						recording_progressing: current
					}
				}
			]
		};
	});
}

export async function mockCameraFleet(
	page: Page,
	count = 127,
	options: { healthGate?: Promise<void>; healthError?: string } = {}
): Promise<void> {
	await mockControlPeer(page, {
		cameras: cameraFleet(count),
		healthGate: options.healthGate,
		healthError: options.healthError,
		health: {
			status: 'degraded',
			health_contract_version: 1,
			generated_at_ms: 1,
			uptime_seconds: 1,
			version: 'test',
			totals: {
				configured_cameras: count,
				connected_cameras: Math.max(0, count - 1),
				fresh_cameras: Math.max(0, count - 1),
				decodable_cameras: Math.max(0, count - 1),
				recording_requested_cameras: count,
				recording_cameras: Math.max(0, count - 1),
				unknown_cameras: 0,
				configured_video_streams: count * 2,
				connected_video_streams: Math.max(0, (count - 1) * 2),
				fresh_video_streams: Math.max(0, count - 1),
				decodable_video_streams: Math.max(0, count - 1),
				recording_requested_video_streams: count,
				recording_video_streams: Math.max(0, count - 1)
			},
			cameras: cameraFleetHealth(count),
			issues: []
		}
	});
}
