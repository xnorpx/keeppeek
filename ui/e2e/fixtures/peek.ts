import type { Page } from '@playwright/test';
import type { CameraListItem } from '../../src/lib/types';
import { mockControlPeer, type HealthFixture } from './control-peer';

function camera(id: string, name: string): CameraListItem {
	return {
		id,
		ip: `192.0.2.${id.length}`,
		name,
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
}

export const mixedCameras = [
	camera('front-door', 'Front Door'),
	camera('porch', 'Porch'),
	camera('alley', 'Alley'),
	camera('back-yard', 'Back Yard')
];

export const mixedHealth: HealthFixture = {
	status: 'degraded',
	health_contract_version: 1,
	totals: {
		configured_cameras: 4,
		connected_cameras: 3,
		fresh_cameras: 2,
		decodable_cameras: 2,
		recording_requested_cameras: 4,
		recording_cameras: 1,
		unknown_cameras: 0,
		configured_video_streams: 4,
		connected_video_streams: 3,
		fresh_video_streams: 2,
		decodable_video_streams: 2,
		recording_requested_video_streams: 4,
		recording_video_streams: 1
	},
	system: {
		system_cpu_percent: 24.8,
		memory: {
			total_bytes: 32_000_000_000,
			used_bytes: 6_100_000_000
		},
		process: {
			cpu_capacity_percent: 3.7,
			resident_memory_bytes: 286_000_000
		}
	},
	cameras: [
		{
			id: 'front-door',
			state: 'healthy',
			reason: 'healthy',
			reason_codes: ['healthy'],
			detail: 'Transport, media, keyframe, and recording evidence is current',
			dimensions: {
				configured: true,
				expected: true,
				transport_connected: true,
				frames_fresh: true,
				decodable: true,
				recording_requested: true,
				recording_video_stream_ids: ['sub'],
				recording_progressing_stream_ids: ['sub'],
				recording_progressing: true,
				session_duration_ms: 600_000,
				recorded_main_duration_ms: 480_000,
				recorded_sub_duration_ms: 300_000,
				recorded_total_duration_ms: 780_000
			},
			lifecycle: 'Connected',
			last_error: null,
			streams: [
				{ type: 'sub', fps: 25, frames: 1_000, drops: 0, report_age_ms: 20, updated_at_ms: 1 }
			]
		},
		{
			id: 'porch',
			state: 'degraded',
			reason: 'ingress_drops',
			reason_codes: ['ingress_drops'],
			detail: '14% frames dropped',
			dimensions: {
				configured: true,
				expected: true,
				transport_connected: true,
				frames_fresh: true,
				decodable: true,
				recent_drops: 14,
				recording_requested: true,
				recording_video_stream_ids: ['sub'],
				recording_progressing_stream_ids: [],
				recording_progressing: false,
				session_duration_ms: 600_000,
				recorded_main_duration_ms: 0,
				recorded_sub_duration_ms: 240_000,
				recorded_total_duration_ms: 240_000
			},
			lifecycle: 'Connected',
			last_error: null,
			streams: [
				{ type: 'sub', fps: 11, frames: 86, drops: 14, report_age_ms: 4_000, updated_at_ms: 1 }
			]
		},
		{
			id: 'alley',
			state: 'stale',
			reason: 'stream_report_stale',
			reason_codes: ['stream_report_stale'],
			detail: 'Stream health report is stale',
			dimensions: {
				configured: true,
				expected: true,
				transport_connected: true,
				frames_fresh: false,
				decodable: false,
				recording_requested: true,
				recording_video_stream_ids: ['sub'],
				recording_progressing_stream_ids: [],
				recording_progressing: false
			},
			lifecycle: 'Attempt 3',
			last_error: null,
			streams: [
				{ type: 'sub', fps: 0, frames: 0, drops: 0, report_age_ms: 41_000, updated_at_ms: 1 }
			]
		},
		{
			id: 'back-yard',
			state: 'offline',
			reason: 'transport_disconnected',
			reason_codes: ['transport_disconnected'],
			detail: 'Authentication failed',
			dimensions: {
				configured: true,
				expected: true,
				transport_connected: false,
				frames_fresh: false,
				decodable: false,
				recording_requested: true,
				recording_video_stream_ids: ['sub'],
				recording_progressing_stream_ids: [],
				recording_progressing: false
			},
			lifecycle: 'Stopped',
			last_error: 'Authentication failed',
			streams: [{ type: 'sub', frames: 0, drops: 0, report_age_ms: 8_040_000, updated_at_ms: 1 }]
		}
	]
};

export async function mockMixedHealth(page: Page): Promise<void> {
	await mockControlPeer(page, { cameras: mixedCameras, health: mixedHealth });
}
