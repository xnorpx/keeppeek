import { describe, expect, it } from 'vitest';
import type { CameraHealth, CameraListItem, StreamHealth } from './types';
import { presentPeekCamera, reconcilePeekCameraPlayback } from './peek-camera';

const camera: CameraListItem = {
	id: 'front-door',
	ip: '192.0.2.10',
	name: 'Front Door',
	manufacturer: null,
	model: null,
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
	profiles: []
};

const stream: StreamHealth = {
	type: 'sub',
	fps: 11,
	frames: 86,
	drops: 14,
	updated_at_ms: 1_000,
	report_age_ms: 41_000
};

function health(state: CameraHealth['state'], overrides: Partial<CameraHealth> = {}): CameraHealth {
	return {
		id: camera.id,
		ip: camera.ip,
		name: camera.name!,
		manufacturer: null,
		model: null,
		firmware_version: null,
		state,
		lifecycle: null,
		last_error: null,
		configured_profiles: [],
		streams: [stream],
		...overrides
	};
}

describe('Peek camera presentation', () => {
	it('shows an online recording stream as live', () => {
		expect(presentPeekCamera(camera, health('online'))).toEqual({
			state: 'live',
			detail: null,
			fps: 11,
			recording: true
		});
	});

	it('derives a degraded frame-drop reason from counters', () => {
		expect(presentPeekCamera(camera, health('degraded'))).toMatchObject({
			state: 'degraded',
			detail: '14% frames dropped',
			fps: 11,
			recording: true
		});
	});

	it('keeps a reconnecting stale lifecycle as reconnecting', () => {
		expect(presentPeekCamera(camera, health('stale', { lifecycle: 'Reconnecting' }))).toMatchObject(
			{
				state: 'reconnecting',
				detail: 'Reconnecting',
				recording: false
			}
		);
	});

	it('does not call connected stale telemetry a reconnect', () => {
		expect(presentPeekCamera(camera, health('stale', { lifecycle: 'Connected' }))).toMatchObject({
			state: 'degraded',
			detail: 'Stream health report stale',
			recording: false
		});
	});

	it('preserves stale server evidence while decoded frames remain active', () => {
		const stale = presentPeekCamera(camera, health('stale', { lifecycle: 'Connected' }));
		expect(reconcilePeekCameraPlayback(stale, 'stale', true)).toMatchObject({
			state: 'degraded',
			detail: 'Stream health report stale'
		});
	});

	it('preserves current degraded evidence while decoded frames remain active', () => {
		const degraded = presentPeekCamera(
			camera,
			health('degraded', { last_error: 'RTSP TCP connection closed' })
		);
		expect(reconcilePeekCameraPlayback(degraded, 'degraded', true)).toMatchObject({
			state: 'degraded',
			detail: 'RTSP TCP connection closed'
		});
	});

	it('maps offline health to a non-recording failure', () => {
		expect(
			presentPeekCamera(camera, health('offline', { last_error: 'Authentication failed' }))
		).toEqual({
			state: 'offline',
			detail: 'Authentication failed',
			fps: null,
			recording: false
		});
	});

	it('treats an unreported configured camera as reconnecting without invented timing', () => {
		expect(presentPeekCamera(camera, null)).toEqual({
			state: 'reconnecting',
			detail: 'Waiting for camera health',
			fps: null,
			recording: false
		});
	});
});
