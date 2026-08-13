import type {
	BrowserLiveSessionResponse,
	BrowserLiveSessionStatus,
	BrowserLiveTrackOffer,
	BrowserLiveTrackStatus,
	CameraDetailsResponse,
	CameraListItem,
	CameraSettings,
	CameraSettingsUpdate,
	CameraSettingsUpdateResponse,
	CameraStatsResponse,
	DiscoveredCameraSettings,
	Health,
	LiveQuality,
	LiveSessionResponse,
	LiveSessionStatus,
	LoggingSettings,
	LogSnapshot,
	MotionDetection,
	RecordingEventsResponse,
	RecordingsResponse,
	RestartResponse,
	SanitizedConfig,
	SettingsConfigUpdate,
	SettingsConfigUpdateResponse,
	ServerHealthResponse
} from './types';

async function get<T>(path: string, signal?: AbortSignal): Promise<T> {
	const res = signal ? await fetch(path, { signal }) : await fetch(path);
	if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
	return res.json();
}

async function post<T>(path: string, body: unknown): Promise<T> {
	const res = await fetch(path, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!res.ok) {
		const message = await res.text();
		throw new Error(`${res.status} ${message || res.statusText}`);
	}
	return res.json();
}

async function put<T>(path: string, body: unknown): Promise<T> {
	const res = await fetch(path, {
		method: 'PUT',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!res.ok) {
		const message = await res.text();
		throw new Error(`${res.status} ${message || res.statusText}`);
	}
	return res.json();
}

async function del(path: string): Promise<void> {
	const res = await fetch(path, { method: 'DELETE' });
	if (!res.ok) {
		const message = await res.text();
		throw new Error(`${res.status} ${message || res.statusText}`);
	}
}

async function postEmpty(path: string): Promise<void> {
	const res = await fetch(path, { method: 'POST' });
	if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
}

export function getHealth(): Promise<Health> {
	return get('/health');
}

export function getHealthAt(origin: string): Promise<Health> {
	return get(new URL('/health', origin).toString());
}

export function getCameras(): Promise<CameraListItem[]> {
	return get('/api/cameras');
}

export function getCameraDetails(id: string, signal?: AbortSignal): Promise<CameraDetailsResponse> {
	return get(`/api/cameras/${encodeURIComponent(id)}/details`, signal);
}

export function setCameraMotionDetection(id: string, enabled: boolean): Promise<MotionDetection> {
	return post(`/api/cameras/${encodeURIComponent(id)}/motion`, { enabled });
}

export function setCameraManufacturer(
	id: string,
	manufacturer: string | null
): Promise<CameraListItem> {
	return put(`/api/cameras/${encodeURIComponent(id)}/manufacturer`, { manufacturer });
}

export function getCameraStats(id: string): Promise<CameraStatsResponse> {
	return get(`/api/cameras/${encodeURIComponent(id)}/stats`);
}

export function getRecordings(
	cameraId: string,
	date?: string,
	signal?: AbortSignal
): Promise<RecordingsResponse> {
	const params = date ? `?date=${encodeURIComponent(date)}` : '';
	return get(`/api/recordings/${encodeURIComponent(cameraId)}${params}`, signal);
}

export function getRecordingEvents(
	cameraId: string,
	date: string
): Promise<RecordingEventsResponse> {
	return get(`/api/events/${encodeURIComponent(cameraId)}?date=${encodeURIComponent(date)}`);
}

export function renewRecordingActivity(cameraId: string, stream: 'main' | 'sub'): Promise<void> {
	return postEmpty(
		`/api/recordings/${encodeURIComponent(cameraId)}/${encodeURIComponent(stream)}/activity`
	);
}

export function getConfig(): Promise<SanitizedConfig> {
	return get('/api/config');
}

export function updateSettingsConfig(
	update: SettingsConfigUpdate
): Promise<SettingsConfigUpdateResponse> {
	return put('/api/settings/config', update);
}

export function getSettingsCameras(): Promise<CameraSettings[]> {
	return get('/api/settings/cameras');
}

export function discoverSettingsCameras(subnets: number[]): Promise<DiscoveredCameraSettings[]> {
	return post('/api/settings/cameras/discover', { subnets });
}

export function updateSettingsCamera(
	ip: string,
	update: CameraSettingsUpdate
): Promise<CameraSettingsUpdateResponse> {
	return put(`/api/settings/cameras/${encodeURIComponent(ip)}`, update);
}

export function removeSettingsCamera(ip: string): Promise<void> {
	return del(`/api/settings/cameras/${encodeURIComponent(ip)}`);
}

export function restartSettingsServer(): Promise<RestartResponse> {
	return post('/api/settings/restart', {});
}

export function getServerHealth(signal?: AbortSignal): Promise<ServerHealthResponse> {
	return get('/api/health', signal);
}

export function getLoggingSettings(signal?: AbortSignal): Promise<LoggingSettings> {
	return get('/api/settings/logging', signal);
}

export function updateLoggingFilter(filter: string): Promise<LoggingSettings> {
	return put('/api/settings/logging', { filter });
}

export function getServerLogs(
	after?: number,
	limit?: number,
	signal?: AbortSignal
): Promise<LogSnapshot> {
	const params = new URLSearchParams();
	if (after !== undefined) params.set('after', String(after));
	if (limit !== undefined) params.set('limit', String(limit));
	const query = params.size > 0 ? `?${params.toString()}` : '';
	return get(`/api/logs${query}`, signal);
}

export function createLiveAnswer(
	cameraId: string,
	stream: 'main' | 'sub',
	offer: RTCSessionDescriptionInit
): Promise<RTCSessionDescriptionInit> {
	return post(`/api/cameras/${encodeURIComponent(cameraId)}/live/${stream}/offer`, offer);
}

export function createLiveSession(
	cameraId: string,
	quality: LiveQuality,
	offer: RTCSessionDescriptionInit
): Promise<LiveSessionResponse> {
	return post(`/api/cameras/${encodeURIComponent(cameraId)}/live/offer`, { quality, offer });
}

export function createBrowserLiveSession(
	tracks: BrowserLiveTrackOffer[],
	offer: RTCSessionDescriptionInit
): Promise<BrowserLiveSessionResponse> {
	return post('/api/live/browser/offer', { tracks, offer });
}

export function getBrowserLiveSessionStatus(sessionId: number): Promise<BrowserLiveSessionStatus> {
	return get(`/api/live/browser/${sessionId}`);
}

export function setBrowserTrackQuality(
	sessionId: number,
	trackId: string,
	quality: LiveQuality
): Promise<BrowserLiveTrackStatus> {
	return post(`/api/live/browser/${sessionId}/tracks/${encodeURIComponent(trackId)}/quality`, {
		quality
	});
}

export function closeBrowserLiveSession(sessionId: number): Promise<void> {
	return fetch(`/api/live/browser/${sessionId}/close`, {
		method: 'POST',
		keepalive: true
	}).then((response) => {
		if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
	});
}

export function closeBrowserLiveSessionOnPageHide(sessionId: number): void {
	const path = `/api/live/browser/${sessionId}/close`;
	if (navigator.sendBeacon(path, new Blob())) return;
	void fetch(path, { method: 'POST', keepalive: true }).catch(() => {});
}

export function setLiveQuality(
	sessionId: number,
	quality: LiveQuality
): Promise<LiveSessionStatus> {
	return post(`/api/live/${sessionId}/quality`, { quality });
}

export function getLiveSessionStatus(sessionId: number): Promise<LiveSessionStatus> {
	return get(`/api/live/${sessionId}`);
}
