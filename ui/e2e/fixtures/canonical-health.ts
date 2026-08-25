import type { CameraListItem } from '../../src/lib/types';
import type { HealthFixture } from './control-peer';

export const canonicalStaleCamera: CameraListItem = {
	id: 'front-door',
	ip: '192.0.2.40',
	name: 'Front Door',
	manufacturer: 'Reolink',
	model: 'RLC-820A',
	firmware_version: 'v1',
	is_reolink: true,
	backend: 'retina',
	transport: 'tcp',
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
			resolution: '1920x1080',
			framerate: 15
		}
	]
};

export const canonicalStaleHealth: HealthFixture = {
	status: 'degraded',
	health_contract_version: 1,
	generated_at_ms: Date.UTC(2026, 7, 10, 12),
	uptime_seconds: 120,
	version: '0.1.0',
	totals: {
		configured_cameras: 1,
		connected_cameras: 1,
		fresh_cameras: 0,
		decodable_cameras: 0,
		recording_requested_cameras: 0,
		recording_cameras: 0,
		unknown_cameras: 0,
		configured_video_streams: 1,
		connected_video_streams: 1,
		fresh_video_streams: 0,
		decodable_video_streams: 0,
		recording_requested_video_streams: 0,
		recording_video_streams: 0
	},
	cameras: [
		{
			id: 'front-door',
			ip: '192.0.2.40',
			name: 'Front Door',
			manufacturer: 'Reolink',
			model: 'RLC-820A',
			firmware_version: 'v1',
			backend: 'retina',
			transport: 'tcp',
			state: 'stale',
			reason: 'frames_not_arriving',
			reason_codes: ['frames_not_arriving'],
			detail: 'Video frames are not arriving',
			dimensions: {
				configured: true,
				expected: true,
				configured_video_streams: 1,
				connected_video_streams: 1,
				reporting_video_streams: 1,
				fresh_video_streams: 0,
				decodable_video_streams: 0,
				configured_video_stream_ids: ['main'],
				connected_video_stream_ids: ['main'],
				reporting_video_stream_ids: ['main'],
				fresh_video_stream_ids: [],
				decodable_video_stream_ids: [],
				transport_connected: true,
				report_age_ms: 100,
				frames_fresh: false,
				decodable: false,
				recent_reconnects: 0,
				recent_drops: 0,
				recent_errors: 0,
				recording_requested: false,
				recording_video_streams: 0,
				recording_streams_progressing: 0
			},
			lifecycle: 'connected',
			last_error: null,
			configured_profiles: canonicalStaleCamera.profiles,
			streams: [
				{
					type: 'video_main',
					codec: 'h264',
					resolution: '1920x1080',
					fps: 0,
					expected_fps: 15,
					frames: 0,
					keyframes: 0,
					updated_at_ms: Date.UTC(2026, 7, 10, 12),
					report_age_ms: 100,
					state: 'stale',
					reason: 'frames_not_arriving',
					reason_codes: ['frames_not_arriving'],
					detail: 'Video frames are not arriving',
					dimensions: {
						expected: true,
						transport_connected: true,
						report_fresh: true,
						frames_fresh: false,
						decodable: false,
						recent_reconnects: 0,
						recent_drops: 0,
						recent_errors: 0,
						recording_requested: false
					}
				}
			]
		}
	],
	issues: [
		{
			severity: 'warning',
			scope: 'Front Door',
			message: 'Video frames are not arriving'
		}
	]
};
