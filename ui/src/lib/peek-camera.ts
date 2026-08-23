import type { CameraHealth, CameraListItem, StreamHealth } from './types';

export type PeekCameraState = 'degraded' | 'live' | 'offline' | 'reconnecting';

export type PeekCameraPresentation = {
	state: PeekCameraState;
	detail: string | null;
	fps: number | null;
	recording: boolean;
};

export function reconcilePeekCameraPlayback(
	presentation: PeekCameraPresentation,
	healthState: CameraHealth['state'] | null,
	hasRecentFrames: boolean
): PeekCameraPresentation {
	if (!hasRecentFrames || presentation.state !== 'reconnecting' || healthState === 'stale') {
		return presentation;
	}
	return {
		...presentation,
		state: 'live',
		detail: null
	};
}

function latestStream(streams: readonly StreamHealth[]): StreamHealth | null {
	return streams.reduce<StreamHealth | null>((latest, stream) => {
		if (latest === null || stream.report_age_ms < latest.report_age_ms) return stream;
		return latest;
	}, null);
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
	const recording = camera.capabilities?.recording === true && health?.state !== 'offline';

	if (health === null) {
		return {
			state: 'reconnecting',
			detail: 'Waiting for camera health',
			fps: null,
			recording: false
		};
	}

	switch (health.state) {
		case 'online':
			return { state: 'live', detail: null, fps: stream?.fps ?? null, recording };
		case 'degraded':
			return {
				state: 'degraded',
				detail: health.last_error ?? dropDetail(health.streams) ?? 'Stream health degraded',
				fps: stream?.fps ?? null,
				recording
			};
		case 'starting':
			return {
				state: 'reconnecting',
				detail: health.last_error ?? health.lifecycle ?? 'Starting camera',
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
				fps: stream?.fps ?? null,
				recording: false
			};
		case 'offline':
			return {
				state: 'offline',
				detail: health.last_error ?? 'Not recording',
				fps: null,
				recording: false
			};
	}
}
