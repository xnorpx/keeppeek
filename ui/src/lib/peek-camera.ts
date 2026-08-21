import type { CameraHealth, CameraListItem, StreamHealth } from './types';

export type PeekCameraState = 'degraded' | 'live' | 'offline' | 'reconnecting';

export type PeekCameraPresentation = {
	state: PeekCameraState;
	detail: string | null;
	lastFrame: string | null;
	fps: number | null;
	recording: boolean;
};

export function reconcilePeekCameraPlayback(
	presentation: PeekCameraPresentation,
	healthState: CameraHealth['state'] | null,
	hasRecentFrames: boolean
): PeekCameraPresentation {
	if (!hasRecentFrames || (presentation.state !== 'reconnecting' && healthState !== 'stale')) {
		return presentation;
	}
	return {
		...presentation,
		state: 'live',
		detail: null,
		lastFrame: null
	};
}

export function peekStreamEvidenceLabel(
	presentation: PeekCameraPresentation,
	stream: 'main' | 'sub',
	hasRecentFrames: boolean
): string {
	if (presentation.fps !== null) {
		return `${stream.toUpperCase()} · ${Math.round(presentation.fps)}FPS`;
	}
	if (hasRecentFrames) return `${stream.toUpperCase()} · LIVE`;
	if (presentation.state === 'reconnecting') return 'WAITING FOR VIDEO';
	if (presentation.state === 'offline') return '';
	return `${stream.toUpperCase()} · FPS —`;
}

function latestStream(streams: readonly StreamHealth[]): StreamHealth | null {
	return streams.reduce<StreamHealth | null>((latest, stream) => {
		if (latest === null || stream.report_age_ms < latest.report_age_ms) return stream;
		return latest;
	}, null);
}

function formatAge(ageMs: number): string {
	if (ageMs < 1_000) return 'last frame just now';
	const seconds = Math.floor(ageMs / 1_000);
	if (seconds < 60) return `last frame ${seconds}s ago`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `last frame ${minutes}m ago`;
	const hours = Math.floor(minutes / 60);
	const remainingMinutes = minutes % 60;
	return `last frame ${hours}h ${remainingMinutes}m ago`;
}

function dropDetail(streams: readonly StreamHealth[]): string | null {
	const frames = streams.reduce((total, stream) => total + (stream.frames ?? 0), 0);
	const drops = streams.reduce((total, stream) => total + (stream.drops ?? 0), 0);
	const observed = frames + drops;
	if (drops === 0 || observed === 0) return null;
	return `${Math.round((drops / observed) * 100)}% frames dropped`;
}

export function presentPeekCamera(
	camera: CameraListItem,
	health: CameraHealth | null
): PeekCameraPresentation {
	const stream = health === null ? null : latestStream(health.streams);
	const lastFrame = stream === null ? null : formatAge(stream.report_age_ms);
	const recording = camera.capabilities?.recording === true && health?.state !== 'offline';

	if (health === null) {
		return {
			state: 'reconnecting',
			detail: 'Waiting for camera health',
			lastFrame: null,
			fps: null,
			recording: false
		};
	}

	switch (health.state) {
		case 'online':
			return { state: 'live', detail: null, lastFrame, fps: stream?.fps ?? null, recording };
		case 'degraded':
			return {
				state: 'degraded',
				detail: health.last_error ?? dropDetail(health.streams) ?? 'Stream health degraded',
				lastFrame,
				fps: stream?.fps ?? null,
				recording
			};
		case 'starting':
			return {
				state: 'reconnecting',
				detail: health.last_error ?? health.lifecycle ?? 'Starting camera',
				lastFrame,
				fps: stream?.fps ?? null,
				recording: false
			};
		case 'stale':
			const lifecycle = health.lifecycle?.trim() ?? '';
			const reconnecting = /reconnect|attempt|starting/i.test(lifecycle);
			return {
				state: reconnecting ? 'reconnecting' : 'degraded',
				detail:
					health.last_error ??
					(reconnecting ? lifecycle || 'Reconnecting' : 'Stream health report stale'),
				lastFrame,
				fps: stream?.fps ?? null,
				recording: false
			};
		case 'offline':
			return {
				state: 'offline',
				detail: health.last_error ?? 'Not recording',
				lastFrame,
				fps: null,
				recording: false
			};
	}
}
