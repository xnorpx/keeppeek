import type {
	CameraRecordingCoverage,
	RecordingCoverageState,
	RecordingGapCause,
	RecordingWriterState
} from './types';

export type CameraCoverageSummary = {
	coveragePercent: number | null;
	recordingBytes: number;
	estimatedBytesPerDay: number;
	effectiveRetentionMs: number | null;
	gapCount: number;
	largestGapMs: number;
};

export function summarizeCamera(camera: CameraRecordingCoverage): CameraCoverageSummary {
	const requested = camera.streams.filter((stream) => stream.recording_requested);
	const measured = requested.filter((stream) => stream.playable_fragments > 0);
	const retentionValues = measured.flatMap((stream) => stream.effective_retention_ms ?? []);
	return {
		coveragePercent:
			measured.length === 0 ? null : Math.min(...measured.map((stream) => stream.coverage_percent)),
		recordingBytes: camera.streams.reduce((total, stream) => total + stream.recording_bytes, 0),
		estimatedBytesPerDay: camera.streams.reduce(
			(total, stream) => total + stream.estimated_bytes_per_day,
			0
		),
		effectiveRetentionMs: retentionValues.length === 0 ? null : Math.min(...retentionValues),
		gapCount: requested.reduce((total, stream) => total + stream.gap_count, 0),
		largestGapMs: requested.reduce((largest, stream) => Math.max(largest, stream.largest_gap_ms), 0)
	};
}

export function formatBytes(bytes: number | null): string {
	if (bytes === null) return 'No evidence';
	if (bytes < 1_000) return `${bytes} B`;
	const units = ['kB', 'MB', 'GB', 'TB', 'PB'];
	let value = bytes / 1_000;
	let unit = 0;
	while (value >= 1_000 && unit < units.length - 1) {
		value /= 1_000;
		unit += 1;
	}
	return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
}

export function formatDuration(durationMs: number | null): string {
	if (durationMs === null) return 'No evidence';
	const totalSeconds = Math.max(0, Math.round(durationMs / 1_000));
	const days = Math.floor(totalSeconds / 86_400);
	const hours = Math.floor((totalSeconds % 86_400) / 3_600);
	const minutes = Math.floor((totalSeconds % 3_600) / 60);
	const seconds = totalSeconds % 60;
	if (days > 0) return `${days}d ${hours}h`;
	if (hours > 0) return `${hours}h ${minutes}m`;
	if (minutes > 0) return `${minutes}m ${seconds}s`;
	return `${seconds}s`;
}

export function formatTimestamp(timestampMs: number | null): string {
	if (timestampMs === null) return 'No evidence';
	return new Intl.DateTimeFormat(undefined, {
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	}).format(new Date(timestampMs));
}

export function formatAge(timestampMs: number | null, nowMs: number): string {
	if (timestampMs === null) return 'No evidence';
	return `${formatDuration(Math.max(0, nowMs - timestampMs))} ago`;
}

export function formatPercent(value: number | null): string {
	return value === null ? 'No evidence' : `${value.toFixed(value >= 99.95 ? 1 : 2)}%`;
}

export function coverageStateLabel(state: RecordingCoverageState): string {
	return {
		healthy: 'Recording healthy',
		degraded: 'Recording degraded',
		paused_by_policy: 'Paused by policy',
		not_configured: 'Not configured',
		unknown: 'Unknown'
	}[state];
}

export function writerStateLabel(state: RecordingWriterState): string {
	return {
		progressing: 'Progressing',
		stalled: 'Stalled',
		failed: 'Failed',
		pending: 'Pending',
		policy_disabled: 'Not requested',
		unknown: 'Unknown'
	}[state];
}

export function gapCauseLabel(cause: RecordingGapCause): string {
	return {
		source_silence: 'Source silence',
		transport_outage: 'Transport outage',
		stale_frames: 'Stale frames',
		decode_failure: 'Decode failure',
		writer_failure: 'Writer failure',
		disk_pressure: 'Disk pressure',
		retention_deletion: 'Retention deletion',
		migration: 'Storage migration',
		catalog_mismatch: 'Catalog mismatch',
		unknown: 'Unknown cause'
	}[cause];
}

export function rangePosition(
	startMs: number,
	endMs: number,
	windowStartMs: number,
	windowEndMs: number
): { left: number; width: number } {
	const duration = Math.max(1, windowEndMs - windowStartMs);
	const start = Math.max(windowStartMs, Math.min(windowEndMs, startMs));
	const end = Math.max(start, Math.min(windowEndMs, endMs));
	return {
		left: ((start - windowStartMs) / duration) * 100,
		width: ((end - start) / duration) * 100
	};
}
