import type { CameraHealth, CameraHealthState, CameraListItem, StreamHealth } from './types';

export type PeekCameraState = CameraHealthState;

export type PeekCameraPresentation = {
	state: PeekCameraState;
	detail: string | null;
	fps: number | null;
	recording: boolean;
};

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
	_camera: CameraListItem,
	health: CameraHealth | null
): PeekCameraPresentation {
	const stream = health === null ? null : latestStream(health.streams);
	const recording = health?.dimensions?.recording_progressing === true;

	if (health === null) {
		return {
			state: 'unknown',
			detail: 'Camera health evidence is unavailable',
			fps: null,
			recording: false
		};
	}

	const detail =
		health.detail ??
		health.last_error ??
		(health.state === 'degraded' ? dropDetail(health.streams) : null) ??
		(health.state === 'healthy' ? null : `Camera health is ${health.state}`);
	return {
		state: health.state,
		detail,
		fps: health.state === 'offline' || health.state === 'stopped' ? null : (stream?.fps ?? null),
		recording
	};
}
