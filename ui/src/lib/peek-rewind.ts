import type { CameraHealth } from './types';

export const peekRewindMaximumSeconds = 120;

export function peekRewindSeconds(distancePx: number, tileHeightPx: number): number {
	if (distancePx <= 0 || tileHeightPx <= 0) return 0;
	return Math.min(
		peekRewindMaximumSeconds,
		Math.round((distancePx / tileHeightPx) * peekRewindMaximumSeconds)
	);
}

export function peekRewindAnchorMs(health: CameraHealth | null, nowMs: number): number {
	if (
		health === null ||
		health.state === 'online' ||
		health.state === 'degraded' ||
		health.streams.length === 0
	) {
		return nowMs;
	}
	const reportAgeMs = Math.min(...health.streams.map((stream) => stream.report_age_ms));
	return Math.max(0, nowMs - reportAgeMs);
}

export function peekRewindTargetMs(anchorMs: number, rewindSeconds: number): number {
	const boundedSeconds = Math.min(peekRewindMaximumSeconds, Math.max(0, Math.round(rewindSeconds)));
	return Math.max(0, anchorMs - boundedSeconds * 1_000);
}
