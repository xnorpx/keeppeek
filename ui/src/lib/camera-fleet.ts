import type { CameraHealth, CameraListItem } from './types';
import { presentPeekCamera, type PeekCameraState } from './peek-camera';

export type CameraFleetPresentation = {
	state: PeekCameraState;
	statusDetail: string;
	transport: string;
	transportDetail: string;
	streams: readonly string[];
	recording: string;
	recordingState: 'healthy' | 'degraded' | 'unknown';
	throughput: string | null;
	gbPerDay: string | null;
};

function formatStreams(camera: CameraListItem, health: CameraHealth | null): string[] {
	const measured = (health?.streams ?? []).filter(
		(stream) => stream.type === 'main' || stream.type === 'sub' || stream.type.startsWith('video_')
	);
	if (measured.length > 0) {
		return measured.map((stream) => {
			const role = stream.type.replace(/^video_/, '');
			const declared = camera.profiles.find((profile) => profile.stream === role);
			return [role, stream.resolution ?? declared?.resolution, stream.codec ?? declared?.encoding]
				.filter((value): value is string => Boolean(value))
				.join(' ')
				.toUpperCase();
		});
	}
	return camera.profiles.map((profile) =>
		[profile.stream, profile.resolution, profile.encoding]
			.filter((value): value is string => Boolean(value))
			.join(' ')
			.toUpperCase()
	);
}

function totalKbps(health: CameraHealth | null): number | null {
	const measured = health?.streams.flatMap((stream) =>
		stream.kbps === undefined ? [] : [stream.kbps]
	);
	if (measured === undefined || measured.length === 0) return null;
	return measured.reduce((total, kbps) => total + kbps, 0);
}

function formatThroughput(kbps: number | null): string | null {
	if (kbps === null) return null;
	return kbps >= 1_000 ? `${(kbps / 1_000).toFixed(1)} Mb/s` : `${Math.round(kbps)} kb/s`;
}

function formatGbPerDay(kbps: number | null): string | null {
	if (kbps === null) return null;
	return ((kbps * 1_000 * 86_400) / 8 / 1_000_000_000).toFixed(1);
}

export function presentCameraFleetRow(
	camera: CameraListItem,
	health: CameraHealth | null
): CameraFleetPresentation {
	const peek = presentPeekCamera(camera, health);
	const kbps = totalKbps(health);
	const transport = camera.backend ?? health?.backend ?? camera.manufacturer ?? 'Not reported';
	const transportDetail = camera.transport ?? health?.transport ?? 'Transport not reported';

	return {
		state: peek.state,
		statusDetail:
			peek.state === 'live'
				? 'ONLINE'
				: `${peek.state.toUpperCase()}${peek.detail ? ` · ${peek.detail}` : ''}`,
		transport,
		transportDetail,
		streams: formatStreams(camera, health),
		recording:
			camera.capabilities?.recording !== true
				? 'Not reported'
				: peek.state === 'offline'
					? 'Not recording'
					: peek.state === 'degraded'
						? 'Gaps reported'
						: 'Continuous',
		recordingState:
			camera.capabilities?.recording !== true
				? 'unknown'
				: peek.state === 'live'
					? 'healthy'
					: 'degraded',
		throughput: formatThroughput(kbps),
		gbPerDay: formatGbPerDay(kbps)
	};
}
