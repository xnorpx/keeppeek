import type { Page } from '@playwright/test';
import type {
	CameraListItem,
	ConfigurationSnapshot,
	ConfigurationTemplate
} from '../../src/lib/types';
import {
	mockControlPeer,
	type ControlRequests,
	type HealthFixture,
	type MockControlPeerOptions
} from './control-peer';

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

export function cameraFleetConfiguration(
	count = 3,
	revision = 'configuration-revision-1',
	templates: ConfigurationTemplate[] = []
): ConfigurationSnapshot {
	return {
		contract_version: 1,
		configuration_revision: revision,
		defaults: {
			username_configured: true,
			password_configured: true,
			configured_backend: null,
			effective_backend: 'auto',
			configured_transport: 'tcp',
			effective_transport: 'tcp',
			configured_record_generic_motion_events: false,
			effective_record_generic_motion_events: false,
			configured_recording_mode: 'event-boost',
			effective_recording_mode: 'event-boost',
			configured_event_recording_duration_secs: 60,
			effective_event_recording_duration_secs: 60
		},
		cameras: cameraFleet(count).map((camera, index) => ({
			camera: {
				id: camera.id,
				ip: camera.ip,
				display_name: camera.name,
				manufacturer_override: null,
				username_configured: true,
				password_configured: true,
				onvif_port: 8000,
				http_port: 80,
				main_rtsp_url: null,
				sub_rtsp_url: null,
				uid_configured: false,
				backend: 'auto',
				transport: 'tcp',
				record_generic_motion_events: false,
				recording_mode: 'event-boost',
				event_recording_duration_secs: 60,
				health: index === 1 ? 'degraded' : index === 2 ? 'offline' : 'healthy',
				model: camera.model
			},
			group_ids: [index < 2 ? 'exterior' : 'interior'],
			username: {
				default_configured: true,
				override_configured: false,
				effective_configured: true,
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			password: {
				default_configured: true,
				override_configured: false,
				effective_configured: true,
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			backend: {
				configured_default: null,
				camera_override: null,
				effective: 'auto',
				source: 'built-in',
				runtime_applied: true,
				warning: null
			},
			transport: {
				configured_default: 'tcp',
				camera_override: null,
				effective: 'tcp',
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			record_generic_motion_events: {
				configured_default: false,
				camera_override: null,
				effective: false,
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			recording_mode: {
				configured_default: 'event-boost',
				camera_override: null,
				effective: 'event-boost',
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			event_recording_duration_secs: {
				configured_default: 60,
				camera_override: null,
				effective: 60,
				source: 'default',
				runtime_applied: true,
				warning: null
			}
		})),
		templates,
		limits: {
			maximum_templates: 64,
			maximum_template_name_bytes: 128,
			maximum_template_description_bytes: 1024,
			maximum_plan_targets: 64,
			maximum_import_bytes: 16_384
		},
		domains: [
			{
				domain_id: 'cameras',
				label: 'Camera defaults and fleet',
				owner_path: '/cameras',
				capability_id: 'keeppeek.configuration.v1',
				readable: true,
				mutable: true,
				unavailable_reason: null
			},
			{
				domain_id: 'storage',
				label: 'Storage and retention',
				owner_path: '/settings#storage',
				capability_id: 'keeppeek.runtime-config.v1',
				readable: true,
				mutable: true,
				unavailable_reason: null
			}
		]
	};
}

type CameraFleetMockOptions = {
	healthGate?: Promise<void>;
	healthError?: string;
} & Pick<
	MockControlPeerOptions,
	| 'capabilityIds'
	| 'configurationSnapshots'
	| 'configurationPlanResult'
	| 'configurationApplyResult'
	| 'configurationTemplateResult'
	| 'configurationTemplateResults'
	| 'configurationImportPreview'
	| 'configurationExportDocument'
	| 'configurationApplyConflictRevision'
	| 'configurationApplyGate'
	| 'runtimeConfiguration'
>;

export async function mockCameraFleet(
	page: Page,
	count = 127,
	options: CameraFleetMockOptions = {}
): Promise<ControlRequests> {
	return mockControlPeer(page, {
		cameras: cameraFleet(count),
		capabilityIds: options.capabilityIds,
		configurationSnapshots: options.configurationSnapshots,
		configurationPlanResult: options.configurationPlanResult,
		configurationApplyResult: options.configurationApplyResult,
		configurationTemplateResult: options.configurationTemplateResult,
		configurationTemplateResults: options.configurationTemplateResults,
		configurationImportPreview: options.configurationImportPreview,
		configurationExportDocument: options.configurationExportDocument,
		configurationApplyConflictRevision: options.configurationApplyConflictRevision,
		configurationApplyGate: options.configurationApplyGate,
		runtimeConfiguration: options.runtimeConfiguration,
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
