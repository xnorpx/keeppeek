import type { HealthFixture } from './control-peer';
import type {
	CameraHealthDimensions,
	CameraHealthReason,
	CameraHealthState,
	StreamHealthDimensions
} from '../../src/lib/types';

function reason(state: CameraHealthState): CameraHealthReason {
	if (state === 'healthy') return 'healthy';
	if (state === 'degraded') return 'ingress_drops';
	if (state === 'stale') return 'stream_report_stale';
	if (state === 'reconnecting') return 'transport_reconnecting';
	if (state === 'offline') return 'transport_disconnected';
	return 'evidence_unavailable';
}

function detail(state: CameraHealthState): string {
	if (state === 'healthy') return 'Transport, media, keyframe, and recording evidence is current';
	if (state === 'degraded') return 'Recent ingress frames were dropped';
	if (state === 'stale') return 'One or more stream health reports are stale';
	if (state === 'reconnecting') return 'Camera transport is reconnecting';
	if (state === 'offline') return 'Camera transport is disconnected';
	return 'Camera health evidence is unavailable';
}

function cameraDimensions(state: CameraHealthState): CameraHealthDimensions {
	const connected = state !== 'offline' && state !== 'reconnecting';
	const current = state === 'healthy' || state === 'degraded';
	return {
		transport_connected: connected,
		frames_fresh: current,
		decodable: current,
		recording_requested: true,
		recording_progressing: current,
		fresh_video_streams: current ? 1 : 0,
		decodable_video_streams: current ? 1 : 0
	} as CameraHealthDimensions;
}

function streamDimensions(state: CameraHealthState): StreamHealthDimensions {
	const current = state === 'healthy' || state === 'degraded';
	return {
		expected: true,
		transport_connected: state !== 'offline' && state !== 'reconnecting',
		report_fresh: current,
		frames_fresh: current,
		decodable: current,
		recording_requested: true,
		recording_progressing: current
	} as StreamHealthDimensions;
}

export const diagnosisHealth = {
	health_contract_version: 1,
	cameras: [
		{
			id: 'back-yard',
			ip: '192.0.2.83',
			name: 'Back Yard',
			manufacturer: 'Reolink',
			model: 'RLC-820A',
			firmware_version: 'v1',
			backend: 'retina',
			transport: 'udp',
			state: 'offline',
			reason: reason('offline'),
			reason_codes: [reason('offline')],
			detail: detail('offline'),
			dimensions: cameraDimensions('offline'),
			lifecycle: 'reconnecting',
			last_error: 'Connection refused',
			configured_profiles: [
				{
					name: 'mainStream',
					stream: 'main',
					encoding: 'h265',
					resolution: '3840x2160',
					framerate: 25
				}
			],
			streams: []
		},
		{
			id: 'front-door',
			ip: '192.0.2.10',
			name: 'Front Door',
			manufacturer: 'Reolink',
			model: null,
			firmware_version: null,
			backend: 'reo-proto',
			transport: 'tcp',
			state: 'healthy',
			reason: reason('healthy'),
			reason_codes: [reason('healthy')],
			detail: detail('healthy'),
			dimensions: cameraDimensions('healthy'),
			lifecycle: 'running',
			last_error: null,
			configured_profiles: [],
			streams: []
		},
		{
			id: 'porch',
			ip: '192.0.2.11',
			name: 'Porch',
			manufacturer: 'ONVIF',
			model: null,
			firmware_version: null,
			backend: 'retina',
			transport: 'udp',
			state: 'degraded',
			reason: reason('degraded'),
			reason_codes: [reason('degraded')],
			detail: detail('degraded'),
			dimensions: cameraDimensions('degraded'),
			lifecycle: 'running',
			last_error: null,
			configured_profiles: [],
			streams: []
		}
	],
	issues: [
		{
			severity: 'warning',
			scope: 'Back Yard',
			message: 'Camera transport is disconnected'
		},
		{
			severity: 'warning',
			scope: 'Porch',
			message: 'Recent ingress frames were dropped'
		}
	]
} satisfies HealthFixture;

export const diagnosisVisualHealth = {
	health_contract_version: 1,
	generated_at_ms: Date.parse('2026-08-18T06:37:07Z'),
	cameras: [
		{
			id: 'back-yard',
			ip: '192.168.1.58',
			name: 'Back Yard',
			manufacturer: 'Reolink',
			model: 'RLC-820A',
			firmware_version: 'v1',
			backend: 'retina',
			transport: 'udp',
			state: 'offline',
			reason: reason('offline'),
			reason_codes: [reason('offline')],
			detail: detail('offline'),
			dimensions: cameraDimensions('offline'),
			lifecycle: 'reconnecting',
			last_error: 'Connection refused',
			configured_profiles: [
				{
					name: 'mainStream',
					stream: 'main',
					encoding: 'h265',
					resolution: '3840x2160',
					framerate: 25
				},
				{
					name: 'subStream',
					stream: 'sub',
					encoding: 'h264',
					resolution: '640x360',
					framerate: 15
				}
			],
			streams: [
				{
					type: 'main',
					frames: 8_420,
					drops: 24,
					errors: 1,
					reconnects: 27,
					updated_at_ms: Date.parse('2026-08-18T04:23:07.412Z'),
					report_age_ms: 8_040_000,
					state: 'offline',
					reason: reason('offline'),
					reason_codes: [reason('offline')],
					detail: detail('offline'),
					dimensions: streamDimensions('offline')
				}
			]
		},
		{
			id: 'porch',
			ip: '192.168.1.59',
			name: 'Porch',
			manufacturer: 'ONVIF',
			model: null,
			firmware_version: null,
			backend: 'retina',
			transport: 'udp',
			state: 'degraded',
			reason: reason('degraded'),
			reason_codes: [reason('degraded')],
			detail: detail('degraded'),
			dimensions: { ...cameraDimensions('degraded'), recent_drops: 184_000 },
			lifecycle: 'running',
			last_error: null,
			configured_profiles: [],
			streams: [
				{
					type: 'main',
					frames: 1_130_000,
					drops: 184_000,
					recent_drops: 184_000,
					reconnects: 3,
					updated_at_ms: Date.parse('2026-08-18T06:37:06.400Z'),
					report_age_ms: 600,
					state: 'degraded',
					reason: reason('degraded'),
					reason_codes: [reason('degraded')],
					detail: detail('degraded'),
					dimensions: { ...streamDimensions('degraded'), recent_drops: 184_000 }
				}
			]
		},
		...Array.from({ length: 41 }, (_, index) => ({
			id: `camera-${index + 1}`,
			ip: `192.168.2.${index + 1}`,
			name: `Camera ${index + 1}`,
			manufacturer: null,
			model: null,
			firmware_version: null,
			backend: 'retina',
			transport: 'tcp',
			state: 'healthy' as const,
			reason: reason('healthy'),
			reason_codes: [reason('healthy')],
			detail: detail('healthy'),
			dimensions: cameraDimensions('healthy'),
			lifecycle: 'running',
			last_error: null,
			configured_profiles: [],
			streams: []
		}))
	],
	issues: [
		{
			severity: 'warning',
			scope: 'Back Yard',
			message: 'Camera transport is disconnected'
		},
		{
			severity: 'warning',
			scope: 'Porch',
			message: 'Recent ingress frames were dropped'
		}
	]
} satisfies HealthFixture;
