import { describe, expect, it } from 'vitest';
import type { CameraHealth, CameraHealthDimensions, CameraListItem } from './types';
import { presentCameraFleetRow } from './camera-fleet';

const camera: CameraListItem = {
	id: 'front-door',
	ip: '192.0.2.10',
	name: 'Front Door',
	manufacturer: 'Reolink',
	model: 'RLC-811A',
	firmware_version: null,
	is_reolink: true,
	backend: 'Reolink',
	transport: 'Baichuan · TCP',
	capabilities: {
		ptz: false,
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
		}
	]
};

function health(
	state: CameraHealth['state'],
	recordingProgressing: boolean | null = true
): CameraHealth {
	return {
		id: camera.id,
		ip: camera.ip,
		name: camera.name ?? camera.id,
		manufacturer: camera.manufacturer,
		model: camera.model,
		firmware_version: null,
		backend: 'Reolink',
		transport: 'Baichuan · TCP',
		state,
		detail: state === 'degraded' ? 'Packet loss' : undefined,
		dimensions: {
			recording_requested: true,
			recording_progressing: recordingProgressing
		} as CameraHealthDimensions,
		lifecycle: 'Connected',
		last_error: state === 'degraded' ? 'Packet loss' : null,
		configured_profiles: camera.profiles,
		streams: [
			{
				type: 'main',
				kbps: 18_400,
				fps: 25,
				frames: 1_000,
				drops: 0,
				updated_at_ms: 1,
				report_age_ms: state === 'offline' ? 8_040_000 : 20
			}
		]
	};
}

describe('camera fleet presentation', () => {
	it('publishes only measured transport, stream, and throughput values', () => {
		expect(presentCameraFleetRow(camera, health('healthy'))).toEqual({
			state: 'healthy',
			statusDetail: 'HEALTHY',
			transport: 'Reolink',
			transportDetail: 'Baichuan · TCP',
			streams: ['MAIN 3840X2160 H265'],
			recording: 'Progressing',
			recordingState: 'healthy',
			throughput: '18.4 Mb/s',
			gbPerDay: '198.7'
		});
	});

	it('keeps degraded media separate from healthy recording progress', () => {
		const presentation = presentCameraFleetRow(camera, health('degraded'));

		expect(presentation.statusDetail).toBe('DEGRADED · Packet loss');
		expect(presentation.recording).toBe('Progressing');
		expect(presentation.recordingState).toBe('healthy');
	});

	it('surfaces requested recording without writer progress', () => {
		const presentation = presentCameraFleetRow(camera, health('degraded', false));

		expect(presentation.recording).toBe('Not progressing');
		expect(presentation.recordingState).toBe('degraded');
	});

	it('prefers measured stream format over a conflicting declared profile', () => {
		const declared = {
			...camera,
			profiles: [{ ...camera.profiles[0], encoding: 'h264' }]
		};
		const measured = health('healthy');
		measured.streams = [
			{
				type: 'video_main',
				codec: 'h265',
				resolution: '3840x2160',
				kbps: 18_400,
				fps: 25,
				frames: 1_000,
				drops: 0,
				updated_at_ms: 1,
				report_age_ms: 20
			}
		];

		expect(presentCameraFleetRow(declared, measured).streams).toEqual(['MAIN 3840X2160 H265']);
	});

	it('does not synthesize metrics when the server omits them', () => {
		const presentation = presentCameraFleetRow(camera, null);

		expect(presentation).toMatchObject({
			state: 'unknown',
			throughput: null,
			gbPerDay: null
		});
	});
});
