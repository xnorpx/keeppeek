import type { CameraHealth, CameraHealthState, CameraListItem, StreamHealth } from './types';

export type PeekCameraState = CameraHealthState;

export type PeekCameraPresentation = {
	state: PeekCameraState;
	detail: string | null;
	fps: number | null;
	recording: boolean;
};

export type PeekRecordingDiagnostics = {
	state: 'recording' | 'not-progressing' | 'pending' | 'off' | 'unknown';
	detail: string;
	sessionDurationMs: number | null;
	mainDurationMs: number | null;
	subDurationMs: number | null;
	totalDurationMs: number | null;
};

export function peekCameraStateColorClass(state: PeekCameraState): string {
	if (state === 'healthy') return 'bg-healthy';
	if (state === 'degraded' || state === 'stale' || state === 'reconnecting') {
		return 'bg-activity';
	}
	if (state === 'offline') return 'bg-live';
	return 'bg-text-muted';
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

function recordingStreamIds(
	health: CameraHealth | null,
	aggregatedIds: readonly string[] | undefined,
	matches: (stream: StreamHealth) => boolean
): string[] {
	if (aggregatedIds && aggregatedIds.length > 0) return [...aggregatedIds];
	return health?.streams.filter(matches).map((stream) => stream.type) ?? [];
}

function formatRecordingStreams(streamIds: readonly string[]): string {
	const names = [...new Set(streamIds.map((streamId) => streamId.replace(/^video_/, '')))].map(
		(streamId) =>
			streamId === 'main'
				? 'Main'
				: streamId === 'sub'
					? 'Sub'
					: streamId.charAt(0).toUpperCase() + streamId.slice(1)
	);
	if (names.length === 0) return 'Stream not reported';
	return `${names.join(' + ')} stream${names.length === 1 ? '' : 's'}`;
}

export function presentPeekRecordingDiagnostics(
	health: CameraHealth | null
): PeekRecordingDiagnostics {
	const dimensions = health?.dimensions;
	if (!dimensions) {
		return {
			state: 'unknown',
			detail: 'Not reported',
			sessionDurationMs: null,
			mainDurationMs: null,
			subDurationMs: null,
			totalDurationMs: null
		};
	}
	const durations = {
		sessionDurationMs: dimensions.session_duration_ms ?? null,
		mainDurationMs: dimensions.recorded_main_duration_ms ?? null,
		subDurationMs: dimensions.recorded_sub_duration_ms ?? null,
		totalDurationMs: dimensions.recorded_total_duration_ms ?? null
	};
	if (!dimensions.recording_requested) {
		return { state: 'off', detail: 'Off', ...durations };
	}

	const requestedStreams = recordingStreamIds(
		health,
		dimensions.recording_video_stream_ids,
		(stream) => stream.dimensions?.recording_requested === true
	);
	const progressingStreams = recordingStreamIds(
		health,
		dimensions.recording_progressing_stream_ids,
		(stream) => stream.dimensions?.recording_progressing === true
	);
	if (progressingStreams.length > 0) {
		const pendingStreams = requestedStreams.filter(
			(streamId) => !progressingStreams.includes(streamId)
		);
		if (pendingStreams.length > 0) {
			return {
				state: 'not-progressing',
				detail: `${formatRecordingStreams(progressingStreams)} recording · ${formatRecordingStreams(pendingStreams)} not progressing`,
				...durations
			};
		}
		return {
			state: 'recording',
			detail: `${formatRecordingStreams(progressingStreams)} · recording`,
			...durations
		};
	}

	const streamLabel = formatRecordingStreams(requestedStreams);
	if (dimensions.recording_progressing === true) {
		return { state: 'recording', detail: `${streamLabel} · recording`, ...durations };
	}
	if (dimensions.recording_progressing === false) {
		return {
			state: 'not-progressing',
			detail: `${streamLabel} · not progressing`,
			...durations
		};
	}
	return { state: 'pending', detail: `${streamLabel} · status pending`, ...durations };
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
