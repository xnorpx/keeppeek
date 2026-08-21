import type { HealthFixture } from './control-peer';

export const diagnosisHealth = {
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
			state: 'online',
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
			message: 'No stream health report has been received'
		},
		{
			severity: 'warning',
			scope: 'Porch',
			message: 'Measured stream FPS is below 70% of the configured rate'
		}
	]
} satisfies HealthFixture;

export const diagnosisVisualHealth = {
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
					report_age_ms: 8_040_000
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
			lifecycle: 'running',
			last_error: null,
			configured_profiles: [],
			streams: [
				{
					type: 'main',
					frames: 1_130_000,
					drops: 184_000,
					reconnects: 3,
					updated_at_ms: Date.parse('2026-08-18T06:37:06.400Z'),
					report_age_ms: 600
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
			state: 'online' as const,
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
			message: 'No stream health report has been received'
		},
		{
			severity: 'warning',
			scope: 'Porch',
			message: 'Measured stream FPS is below 70% of the configured rate'
		}
	]
} satisfies HealthFixture;
