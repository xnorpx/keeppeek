import { describe, expect, it } from 'vitest';
import type { CameraHealth, CameraHealthDimensions, CameraListItem, StreamHealth } from './types';
import { presentPeekCamera } from './peek-camera';

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
	it('shows healthy state without inventing recording progress', () => {
		expect(presentPeekCamera(camera, health('healthy'))).toEqual({
			state: 'healthy',
			detail: null,
			fps: 11,
			recording: false
		});
	});

	it('shows recording only from server writer progress', () => {
		expect(
			presentPeekCamera(
				camera,
				health('healthy', {
					dimensions: { recording_progressing: true } as CameraHealthDimensions
				})
			)
		).toMatchObject({ state: 'healthy', recording: true });
	});

	it('derives a degraded frame-drop reason from counters', () => {
		expect(presentPeekCamera(camera, health('degraded'))).toMatchObject({
			state: 'degraded',
			detail: '14% frames dropped',
			fps: 11,
			recording: false
		});
	});

	it('does not reinterpret stale from lifecycle strings', () => {
		expect(
			presentPeekCamera(
				camera,
				health('stale', {
					lifecycle: 'Reconnecting',
					detail: 'Stream health report stale'
				})
			)
		).toMatchObject({
			state: 'stale',
			detail: 'Stream health report stale',
			recording: false
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

	it('treats unavailable camera evidence as unknown', () => {
		expect(presentPeekCamera(camera, null)).toEqual({
			state: 'unknown',
			detail: 'Camera health evidence is unavailable',
			fps: null,
			recording: false
		});
	});
});
