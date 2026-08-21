import type { CameraHealth, CameraListItem } from './types';
import { presentPeekCamera, type PeekCameraState } from './peek-camera';

export type CameraFleetPresentation = {
	state: PeekCameraState;
	statusDetail: string;
	transport: string;
	transportDetail: string;
	streams: readonly string[];
	recording: string;
	throughput: string | null;
	gbPerDay: string | null;
};

function formatStream(camera: CameraListItem): string[] {
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
		streams: formatStream(camera),
		recording:
			camera.capabilities?.recording !== true || peek.state === 'offline'
				? 'Not recording'
				: peek.state === 'degraded'
					? 'Gaps reported'
					: 'Continuous',
		throughput: formatThroughput(kbps),
		gbPerDay: formatGbPerDay(kbps)
	};
}
