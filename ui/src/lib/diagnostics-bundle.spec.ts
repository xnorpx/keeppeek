import { describe, expect, it } from 'vitest';
import {
	buildDiagnosticsBundle,
	collectReportDiagnosticsBundle,
	diagnosticsBundleFilename
} from './diagnostics-bundle';
import { reportLogWindow } from './report-log-window';
import type { DiagnosticsBundleInput } from './diagnostics-bundle';
import type { CameraListItem, SanitizedConfig, ServerHealthResponse } from './types';

function input(): DiagnosticsBundleInput {
	const health = {
		status: 'degraded',
		generated_at_ms: 1,
		uptime_seconds: 2,
		version: '0.1.0',
		totals: {},
		system: {
			host_name: 'marcus-nvr.local',
			process: {
				executable: '/Users/marcus/private/keeppeek',
				working_directory: '/Users/marcus/private'
			},
			disks: [
				{
					name: 'Marcus Recording Disk',
					mount_point: '/Volumes/Marcus Cameras'
				}
			],
			networks: [{ name: 'Marcus Private VPN' }]
		},
		storage: {
			medium_term_path: '/Volumes/Marcus Cameras/medium',
			long_term_path: '/Volumes/Marcus Cameras/long'
		},
		webrtc: {},
		cameras: [
			{
				id: 'front-door-private',
				ip: '192.168.1.22',
				name: 'Marcus Front Door',
				manufacturer: 'Reolink',
				model: 'RLC-820A',
				firmware_version: 'v1',
				state: 'offline',
				lifecycle: 'reconnecting',
				last_error: 'camera Marcus Front Door at 192.168.1.22 failed',
				configured_profiles: [],
				streams: []
			}
		],
		issues: []
	} as unknown as ServerHealthResponse;
	const config = {
		host: 'marcus-nvr.local',
		port: 8080,
		storage: {
			medium_term_path: '/Volumes/Marcus Cameras/medium',
			long_term_path: '/Volumes/Marcus Cameras/long',
			recording_catalog_path: '/Users/marcus/catalog.db',
			event_thumbnail_path: '/Users/marcus/thumbs'
		},
		camera_count: 1,
		recording_estimate: {}
	} as unknown as SanitizedConfig;
	const cameras = [
		{
			id: 'front-door-private',
			ip: '192.168.1.22',
			name: 'Marcus Front Door',
			serial_number: 'private-serial',
			hardware_id: 'private-hardware-id',
			hostname: 'camera-private.local',
			mac_address: '00:11:22:33:44:55',
			profiles: []
		}
	] as unknown as CameraListItem[];
	return {
		server: {
			entries: [
				{
					sequence: 1,
					timestamp_ms: 1,
					level: 'error',
					target: 'keeppeek::camera',
					message:
						'Marcus Front Door rtsp://operator:camera-secret@192.168.1.22/live token=abc123 grant=private-upload-grant-canary user@example.com',
					fields: {
						password: 'camera-secret',
						session_id: 'private-session',
						grant: 'field-upload-grant-canary',
						path: '/Users/marcus/private/file.mp4'
					}
				}
			],
			oldest_sequence: 1,
			newest_sequence: 1,
			truncated: false,
			stats: {
				entry_count: 1,
				byte_count: 300,
				evicted_entries: 0,
				max_entries: 10_000,
				max_bytes: 8_388_608,
				active_streams: 0,
				max_streams: 8
			}
		},
		browser: [
			{
				sequence: 1,
				timestamp_ms: 2,
				level: 'error',
				target: 'browser.test',
				message: 'Bearer private-browser-token from 192.168.1.22',
				fields: {},
				source: 'console',
				file: '/Users/marcus/app.js'
			}
		],
		health,
		config,
		cameras,
		metrics: 'keeppeek_camera_info{camera="front-door-private",ip="192.168.1.22"} 1\n',
		generatedAt: new Date('2026-08-25T12:00:00.000Z'),
		client: { origin: 'https://marcus-nvr.local', user_agent: 'KeepPeek Test' }
	};
}

describe('diagnostics bundle', () => {
	it('includes the complete evidence set and scrubs private values before compression', () => {
		const contents = buildDiagnosticsBundle(input());
		const document = JSON.parse(contents);
		expect(document.manifest.artifacts).toEqual([
			'server_logs',
			'browser_logs',
			'log_buffer',
			'health',
			'runtime_config',
			'cameras',
			'metrics',
			'browser_environment'
		]);
		for (const privateValue of [
			'Marcus Front Door',
			'front-door-private',
			'192.168.1.22',
			'marcus-nvr.local',
			'/Users/marcus',
			'Marcus Recording Disk',
			'Marcus Private VPN',
			'private-serial',
			'private-hardware-id',
			'camera-private.local',
			'00:11:22:33:44:55',
			'camera-secret',
			'abc123',
			'private-session',
			'private-upload-grant-canary',
			'field-upload-grant-canary',
			'private-browser-token',
			'user@example.com'
		]) {
			expect(contents).not.toContain(privateValue);
		}
		expect(contents).toContain('camera-001');
		expect(contents).toContain('[REDACTED_HOST]');
		expect(contents).toContain('[REDACTED_PATH]');
		expect(contents).toContain('"privacy": "scrubbed"');
		expect(contents).toContain('"redaction_context": "complete"');
		expect(document.server_logs).toHaveLength(1);
		expect(document.browser_logs).toHaveLength(1);
	});

	it.each([
		['health', 'Health evidence is required'],
		['config', 'Runtime configuration is required'],
		['cameras', 'Camera inventory is required']
	] as const)('fails closed when %s scrub context is unavailable', (missing, message) => {
		const incomplete = { ...input(), [missing]: undefined } as unknown as DiagnosticsBundleInput;

		expect(() => buildDiagnosticsBundle(incomplete)).toThrow(message);
	});

	it('uses the generation timestamp in the compressed package filename', () => {
		expect(diagnosticsBundleFilename(new Date('2026-08-25T12:00:00.000Z'))).toBe(
			'keeppeek-diagnostics-2026-08-25T12-00-00-000Z.json.gz'
		);
	});
});

it('packages only selected report-window entries and still scrubs its metadata', () => {
	const fixture = input();
	const report = reportLogWindow(
		{ utcMs: 10, browserSequence: 1, server: { sequence: 1, generation: 'one' } },
		{
			utcMs: 20,
			browserSequence: 1,
			server: { sequence: 1, generation: 'one', filter: '192.168.1.22=debug' }
		},
		fixture.browser,
		fixture.server,
		false
	);
	const serialized = buildDiagnosticsBundle({ ...fixture, report });
	const document = JSON.parse(serialized);
	expect(document.server_logs).toEqual([]);
	expect(document.browser_logs).toEqual([]);
	expect(document.manifest.artifacts).toContain('report_window');
	expect(document.report_window.serverCursors).toEqual({ start: 1, stop: 1 });
	expect(serialized).not.toContain('192.168.1.22');
	expect(JSON.parse(buildDiagnosticsBundle(fixture)).report_window).toBeUndefined();
});

it('identifies each unavailable report section without exposing transport errors in its warnings', async () => {
	const data = input();
	const position = { utcMs: 2, browserSequence: 0, server: { sequence: 1, generation: 'one' } };
	const report = reportLogWindow(position, position, [], data.server, false);
	const unavailable = () => Promise.reject(new Error('private diagnostic detail'));
	const control = {
		getLogSnapshot: async () => data.server,
		getLoggingSettings: unavailable,
		getHealth: async () => data.health,
		getRuntimeConfiguration: async () => data.config,
		getCameras: async () => data.cameras,
		getMetricsSnapshot: unavailable
	};
	const signal = new AbortController().signal;
	const partial = await collectReportDiagnosticsBundle(control, data.server, report, signal);
	expect(partial.diagnostics).toBeInstanceOf(Blob);
	expect(
		partial.redactText?.('Replay front-door-private while saving to /Users/marcus/private')
	).toBe('Replay camera-001 while saving to [REDACTED_PATH]');
	expect(partial.warnings).toEqual([
		'diagnostics-logging-settings-unavailable',
		'diagnostics-metrics-unavailable'
	]);
	const blocked = await collectReportDiagnosticsBundle(
		{ ...control, getCameras: unavailable },
		data.server,
		report,
		signal
	);
	expect(blocked.diagnostics).toBeNull();
	expect(blocked.redactText).toBeNull();
	expect(blocked.warnings).toEqual([
		'diagnostics-logging-settings-unavailable',
		'diagnostics-cameras-unavailable',
		'diagnostics-metrics-unavailable',
		'diagnostics-package-unavailable'
	]);
});
