import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	closeBrowserLiveSession,
	createBrowserLiveSession,
	createLiveAnswer,
	createLiveSession,
	discoverSettingsCameras,
	getCameraStats,
	getHealthAt,
	getCameraDetails,
	getLiveSessionStatus,
	getLoggingSettings,
	getSettingsCameras,
	getRecordingEvents,
	getRecordings,
	getServerHealth,
	getServerLogs,
	renewRecordingActivity,
	restartSettingsServer,
	removeSettingsCamera,
	setBrowserLiveTrackQuality,
	setCameraManufacturer,
	setLiveQuality,
	setCameraMotionDetection,
	updateSettingsCamera,
	updateSettingsConfig,
	updateLoggingFilter
} from './api';

afterEach(() => {
	vi.unstubAllGlobals();
});

function jsonResponse(body: unknown, init: ResponseInit = {}): Response {
	return new Response(JSON.stringify(body), {
		...init,
		headers: { 'Content-Type': 'application/json', ...init.headers }
	});
}

describe('KeepPeek API client', () => {
	it('encodes camera identifiers and recording dates', async () => {
		const fetchMock = vi.fn(async () =>
			jsonResponse({ camera_id: 'front gate', date: '2026/08/10', dates: [], segments: [] })
		);
		vi.stubGlobal('fetch', fetchMock);

		await getRecordings('front gate/1', '2026/08/10');

		expect(fetchMock).toHaveBeenCalledWith('/api/recordings/front%20gate%2F1?date=2026%2F08%2F10');
	});

	it('encodes event camera identifiers and dates', async () => {
		const fetchMock = vi.fn(async () =>
			jsonResponse({ camera_id: 'front gate', date: '2026/08/10', events: [] })
		);
		vi.stubGlobal('fetch', fetchMock);

		await getRecordingEvents('front gate/1', '2026/08/10');

		expect(fetchMock).toHaveBeenCalledWith('/api/events/front%20gate%2F1?date=2026%2F08%2F10');
	});

	it('uses POST for recording activity leases', async () => {
		const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
		vi.stubGlobal('fetch', fetchMock);

		await renewRecordingActivity('yard/north', 'sub');

		expect(fetchMock).toHaveBeenCalledWith('/api/recordings/yard%2Fnorth/sub/activity', {
			method: 'POST'
		});
	});

	it('returns camera statistics from successful responses', async () => {
		const report = { streams: [{ codec: 'h265', fps: 25 }] };
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => jsonResponse({ camera_id: 'yard', report }))
		);

		await expect(getCameraStats('yard')).resolves.toEqual({ camera_id: 'yard', report });
	});

	it('loads details and updates motion detection for a camera', async () => {
		const details = { camera: { id: 'front gate' }, health: null, motion_detection: {} };
		const motion = { supported: true, controllable: true, enabled: false, error: null };
		const fetchMock = vi
			.fn()
			.mockResolvedValueOnce(jsonResponse(details))
			.mockResolvedValueOnce(jsonResponse(motion));
		vi.stubGlobal('fetch', fetchMock);

		await expect(getCameraDetails('front gate')).resolves.toEqual(details);
		await expect(setCameraMotionDetection('front gate', false)).resolves.toEqual(motion);

		expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/cameras/front%20gate/details');
		expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/cameras/front%20gate/motion', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ enabled: false })
		});
	});

	it('saves a manufacturer override for a camera', async () => {
		const camera = { id: 'front gate', manufacturer: 'Hikvision' };
		const fetchMock = vi.fn(async () => jsonResponse(camera));
		vi.stubGlobal('fetch', fetchMock);

		await expect(setCameraManufacturer('front gate', 'Hikvision')).resolves.toEqual(camera);

		expect(fetchMock).toHaveBeenCalledWith('/api/cameras/front%20gate/manufacturer', {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ manufacturer: 'Hikvision' })
		});
	});

	it('returns the comprehensive server health snapshot', async () => {
		const snapshot = { status: 'healthy', cameras: [], issues: [] };
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => jsonResponse(snapshot))
		);

		await expect(getServerHealth()).resolves.toEqual(snapshot);
	});

	it('loads logging settings and an encoded server log cursor', async () => {
		const settings = { active_filter: 'info,str0m=warn' };
		const snapshot = { entries: [], truncated: false };
		const fetchMock = vi
			.fn()
			.mockResolvedValueOnce(jsonResponse(settings))
			.mockResolvedValueOnce(jsonResponse(snapshot));
		vi.stubGlobal('fetch', fetchMock);

		await expect(getLoggingSettings()).resolves.toEqual(settings);
		await expect(getServerLogs(41, 500)).resolves.toEqual(snapshot);

		expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/settings/logging');
		expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/logs?after=41&limit=500');
	});

	it('updates the server log filter', async () => {
		const settings = { active_filter: 'debug,retina=info' };
		const fetchMock = vi.fn(async () => jsonResponse(settings));
		vi.stubGlobal('fetch', fetchMock);

		await expect(updateLoggingFilter('debug,retina=info')).resolves.toEqual(settings);

		expect(fetchMock).toHaveBeenCalledWith('/api/settings/logging', {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ filter: 'debug,retina=info' })
		});
	});

	it('checks health at a changed server origin', async () => {
		const fetchMock = vi.fn(async () => jsonResponse({ status: 'ok' }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(getHealthAt('http://127.0.0.1:3200')).resolves.toEqual({ status: 'ok' });

		expect(fetchMock).toHaveBeenCalledWith('http://127.0.0.1:3200/health');
	});

	it('discovers, updates, removes, and applies camera settings', async () => {
		const fetchMock = vi.fn(async () => jsonResponse({ cameras: [] }));
		vi.stubGlobal('fetch', fetchMock);
		const update = {
			display_name: 'Back Gate',
			username: 'operator',
			password: 'write-only',
			onvif_port: 80,
			backend: 'retina' as const,
			transport: 'tcp' as const
		};

		await getSettingsCameras();
		await discoverSettingsCameras([137, 138]);
		await updateSettingsCamera('192.168.137.7', update);
		await removeSettingsCamera('192.168.137.7');
		await restartSettingsServer();

		expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/settings/cameras');
		expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/settings/cameras/discover', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ subnets: [137, 138] })
		});
		expect(fetchMock).toHaveBeenNthCalledWith(3, '/api/settings/cameras/192.168.137.7', {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify(update)
		});
		expect(fetchMock).toHaveBeenNthCalledWith(4, '/api/settings/cameras/192.168.137.7', {
			method: 'DELETE'
		});
		expect(fetchMock).toHaveBeenNthCalledWith(5, '/api/settings/restart', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({})
		});
	});

	it('saves server and storage settings', async () => {
		const update = {
			host: '127.0.0.1',
			port: 3200,
			move_existing_recordings: true,
			storage: {
				medium_term_path: '/recordings/medium',
				long_term_path: '/recordings/long',
				recording_catalog_path: '/metadata/recordings.db',
				event_thumbnail_path: '/metadata/thumbnails',
				event_thumbnail_max_mb: 512,
				short_term_secs: 30,
				medium_term_secs: 120,
				flush_interval_secs: 15,
				write_buffer_bytes: 16384,
				long_term_max_gb: 24
			}
		};
		const fetchMock = vi.fn(async () => jsonResponse({ config: update, restart_required: true }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(updateSettingsConfig(update)).resolves.toEqual({
			config: update,
			restart_required: true
		});

		expect(fetchMock).toHaveBeenCalledWith('/api/settings/config', {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify(update)
		});
	});

	it('includes server details in failed offer errors', async () => {
		const fetchMock = vi.fn(
			async () => new Response('camera busy', { status: 503, statusText: 'Service Unavailable' })
		);
		vi.stubGlobal('fetch', fetchMock);
		const offer = { type: 'offer', sdp: 'v=0' } satisfies RTCSessionDescriptionInit;

		await expect(createLiveAnswer('front gate', 'main', offer)).rejects.toThrow('503 camera busy');
		expect(fetchMock).toHaveBeenCalledWith('/api/cameras/front%20gate/live/main/offer', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify(offer)
		});
	});

	it('creates and controls adaptive live sessions', async () => {
		const response = {
			session_id: 42,
			answer: { type: 'answer', sdp: 'v=0' },
			requested_quality: 'auto',
			active_stream: 'sub',
			estimated_bitrate_bps: null
		};
		const fetchMock = vi.fn(async () => jsonResponse(response));
		vi.stubGlobal('fetch', fetchMock);
		const offer = { type: 'offer', sdp: 'v=0' } satisfies RTCSessionDescriptionInit;

		await expect(createLiveSession('front gate', 'auto', offer)).resolves.toEqual(response);
		await setLiveQuality(42, 'high');
		await getLiveSessionStatus(42);

		expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/cameras/front%20gate/live/offer', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ quality: 'auto', offer })
		});
		expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/live/42/quality', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ quality: 'high' })
		});
		expect(fetchMock).toHaveBeenNthCalledWith(3, '/api/live/42');
	});

	it('creates and controls shared browser live sessions', async () => {
		const response = {
			session_id: 42,
			answer: { type: 'answer', sdp: 'v=0' }
		};
		const fetchMock = vi.fn(async () => jsonResponse(response));
		vi.stubGlobal('fetch', fetchMock);
		const offer = { type: 'offer', sdp: 'v=0' } satisfies RTCSessionDescriptionInit;
		const tracks = [
			{ camera_id: 'front gate', track_id: 'camera-0', mid: '0', quality: 'low' }
		] as const;

		await expect(createBrowserLiveSession([...tracks], offer)).resolves.toEqual(response);
		await setBrowserLiveTrackQuality(42, 'camera-0', 'high');
		await closeBrowserLiveSession(42);

		expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/live/browser/offer', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ tracks, offer })
		});
		expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/live/browser/42/tracks/camera-0/quality', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ quality: 'high' })
		});
		expect(fetchMock).toHaveBeenNthCalledWith(3, '/api/live/browser/42/close', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({})
		});
	});
});
