import type {
	CreateRequest,
	CreateResponse,
	LogSnapshot,
	RecordingCoverageQuery,
	RecordingCoverageResponse,
	RecordingGap,
	ServerLogEntry,
	StreamRecordingCoverage
} from './types';

export class ApiRequestError extends Error {
	constructor(
		readonly status: number,
		message: string
	) {
		super(message);
		this.name = 'ApiRequestError';
	}
}

function authenticatedHeaders(
	accessKey: string | null | undefined,
	headers: Record<string, string>
): Record<string, string> {
	return accessKey ? { ...headers, Authorization: `Bearer ${accessKey}` } : headers;
}

export async function fetchLogSnapshot(accessKey?: string | null): Promise<LogSnapshot> {
	const response = await fetch('/logs/snapshot', {
		headers: authenticatedHeaders(accessKey, { Accept: 'application/json' }),
		cache: 'no-store'
	});
	if (!response.ok) throw new ApiRequestError(response.status, response.statusText);
	const value: unknown = await response.json();
	if (!isLogSnapshot(value)) throw new Error('Server returned an invalid log snapshot.');
	return value;
}

export async function fetchMetricsSnapshot(accessKey?: string | null): Promise<string> {
	const response = await fetch('/metrics', {
		headers: authenticatedHeaders(accessKey, { Accept: 'text/plain' }),
		cache: 'no-store'
	});
	if (!response.ok) throw new ApiRequestError(response.status, response.statusText);
	return response.text();
}

export async function fetchRecordingCoverage(
	query: RecordingCoverageQuery = {},
	accessKey?: string | null,
	signal?: AbortSignal
): Promise<RecordingCoverageResponse> {
	const parameters = new URLSearchParams();
	if (query.pageToken) {
		parameters.set('page_token', query.pageToken);
	} else {
		if (query.startMs !== undefined) parameters.set('start_ms', String(query.startMs));
		if (query.endMs !== undefined) parameters.set('end_ms', String(query.endMs));
		if (query.minimumGapMs !== undefined) {
			parameters.set('minimum_gap_ms', String(query.minimumGapMs));
		}
		if (query.minimumCameraGapMs !== undefined) {
			parameters.set('minimum_camera_gap_ms', String(query.minimumCameraGapMs));
		}
		if (query.pageSize !== undefined) parameters.set('page_size', String(query.pageSize));
		if (query.search) parameters.set('search', query.search);
		if (query.state) parameters.set('state', query.state);
		if (query.stream) parameters.set('stream', query.stream);
		if (query.group) parameters.set('group', query.group);
	}
	const suffix = parameters.size === 0 ? '' : `?${parameters}`;
	const request: RequestInit = {
		headers: authenticatedHeaders(accessKey, { Accept: 'application/json' }),
		cache: 'no-store'
	};
	if (signal) request.signal = signal;
	const response = await fetch(`/recording-coverage${suffix}`, request);
	if (!response.ok) throw new ApiRequestError(response.status, response.statusText);
	const value: unknown = await response.json();
	if (!isRecordingCoverageResponse(value)) {
		throw new Error('Server returned an invalid recording coverage snapshot.');
	}
	return value;
}

export async function fetchLogStream(
	url: string,
	accessKey: string | null | undefined,
	signal: AbortSignal
): Promise<Response> {
	const response = await fetch(url, {
		headers: authenticatedHeaders(accessKey, { Accept: 'text/event-stream' }),
		cache: 'no-store',
		signal
	});
	if (!response.ok) throw new ApiRequestError(response.status, response.statusText);
	if (!response.body) throw new Error('Server log stream has no response body.');
	return response;
}

async function postEmpty(
	path: string,
	body?: unknown,
	accessKey?: string | null,
	keepalive = false
): Promise<void> {
	const res = await fetch(
		path,
		body === undefined
			? {
					method: 'POST',
					headers: authenticatedHeaders(accessKey, {}),
					keepalive
				}
			: {
					method: 'POST',
					headers: authenticatedHeaders(accessKey, {
						'Content-Type': 'application/json',
						Prefer: 'return=representation'
					}),
					body: JSON.stringify(body),
					keepalive
				}
	);
	if (!res.ok) throw new ApiRequestError(res.status, res.statusText);
	await res.text();
}

export async function waitForMetricsAt(origin: string): Promise<void> {
	const url = new URL('/metrics', origin);
	const crossOrigin = url.origin !== window.location.origin;
	const response = await fetch(url, crossOrigin ? { mode: 'no-cors' } : undefined);
	if (!crossOrigin && !response.ok) throw new Error(`${response.status} ${response.statusText}`);
}

export async function createSession(
	offer: RTCSessionDescriptionInit,
	accessKey?: string | null
): Promise<CreateResponse> {
	const request: CreateRequest = { offer: { type: offer.type as string, sdp: offer.sdp! } };
	const requestString = JSON.stringify(request);

	let body: ArrayBuffer | Uint8Array;
	if (typeof CompressionStream !== 'undefined') {
		const stream = new Blob([requestString])
			.stream()
			.pipeThrough(
				new /* eslint-disable-next-line @typescript-eslint/no-explicit-any */ (
					window as any
				).CompressionStream('gzip')
			);
		body = await new Response(stream).arrayBuffer();
	} else {
		throw new Error('CompressionStream not supported in this environment');
	}

	const res = await fetch('/create', {
		method: 'POST',
		headers: authenticatedHeaders(accessKey, {
			'Content-Type': 'application/json',
			'Content-Encoding': 'gzip'
		}),
		body
	});

	if (!res.ok) {
		throw new ApiRequestError(res.status, res.statusText);
	}

	return res.json();
}

export function deleteSession(
	sessionId: string,
	accessKey?: string | null,
	options: { keepalive?: boolean } = {}
): Promise<void> {
	return postEmpty('/delete', { session_id: sessionId }, accessKey, options.keepalive);
}

function isLogSnapshot(value: unknown): value is LogSnapshot {
	if (!value || typeof value !== 'object') return false;
	const snapshot = value as Partial<LogSnapshot>;
	return (
		Array.isArray(snapshot.entries) &&
		snapshot.entries.every(isServerLogEntry) &&
		(snapshot.oldest_sequence === null || typeof snapshot.oldest_sequence === 'number') &&
		(snapshot.newest_sequence === null || typeof snapshot.newest_sequence === 'number') &&
		typeof snapshot.truncated === 'boolean' &&
		Boolean(snapshot.stats) &&
		typeof snapshot.stats?.entry_count === 'number' &&
		typeof snapshot.stats?.byte_count === 'number' &&
		typeof snapshot.stats?.evicted_entries === 'number' &&
		typeof snapshot.stats?.max_entries === 'number' &&
		typeof snapshot.stats?.max_bytes === 'number'
	);
}

function isServerLogEntry(value: unknown): value is ServerLogEntry {
	if (!value || typeof value !== 'object') return false;
	const entry = value as Partial<ServerLogEntry>;
	return (
		typeof entry.sequence === 'number' &&
		typeof entry.timestamp_ms === 'number' &&
		typeof entry.level === 'string' &&
		typeof entry.target === 'string' &&
		typeof entry.message === 'string' &&
		Boolean(entry.fields) &&
		typeof entry.fields === 'object'
	);
}

export function isRecordingCoverageResponse(value: unknown): value is RecordingCoverageResponse {
	if (!isObject(value)) return false;
	return (
		isFiniteNumber(value.generated_at_ms) &&
		typeof value.catalog_available === 'boolean' &&
		isFiniteNumber(value.catalog_revision) &&
		isNullableNumber(value.catalog_updated_at_ms) &&
		isCoverageWindow(value.window) &&
		isCoverageTotals(value.totals) &&
		isCoverageStorage(value.storage) &&
		Array.isArray(value.groups) &&
		value.groups.every((group) => typeof group === 'string') &&
		Array.isArray(value.cameras) &&
		value.cameras.length <= 50 &&
		value.cameras.every(isCameraRecordingCoverage) &&
		Array.isArray(value.findings) &&
		value.findings.length <= 100 &&
		value.findings.every(isRecordingFinding) &&
		(value.next_page_token === null || typeof value.next_page_token === 'string')
	);
}

function isCoverageWindow(value: unknown): boolean {
	return (
		isObject(value) &&
		isFiniteNumber(value.start_ms) &&
		isFiniteNumber(value.end_ms) &&
		isFiniteNumber(value.minimum_gap_ms)
	);
}

function isCoverageTotals(value: unknown): boolean {
	return (
		isObject(value) &&
		[
			'cameras',
			'healthy',
			'degraded',
			'paused_by_policy',
			'not_configured',
			'unknown',
			'recording_bytes',
			'estimated_bytes_per_day'
		].every((key) => isFiniteNumber(value[key]))
	);
}

function isCoverageStorage(value: unknown): boolean {
	return (
		isObject(value) &&
		typeof value.pressure === 'string' &&
		typeof value.recording_state === 'string' &&
		isNullableNumber(value.available_bytes) &&
		isNullableNumber(value.effective_limit_bytes) &&
		isFiniteNumber(value.recording_bytes) &&
		isFiniteNumber(value.estimated_bytes_per_day) &&
		isNullableNumber(value.projected_retention_days) &&
		typeof value.projection_assumption === 'string'
	);
}

function isCameraRecordingCoverage(value: unknown): boolean {
	return (
		isObject(value) &&
		typeof value.camera_id === 'string' &&
		typeof value.camera_name === 'string' &&
		Array.isArray(value.groups) &&
		value.groups.every((group) => typeof group === 'string') &&
		isCoverageState(value.state) &&
		typeof value.recording_requested === 'boolean' &&
		typeof value.policy === 'string' &&
		Array.isArray(value.streams) &&
		value.streams.every(isStreamRecordingCoverage) &&
		typeof value.health_href === 'string'
	);
}

function isStreamRecordingCoverage(value: unknown): value is StreamRecordingCoverage {
	return (
		isObject(value) &&
		typeof value.stream_id === 'string' &&
		typeof value.recording_stream_id === 'string' &&
		typeof value.recording_requested === 'boolean' &&
		isWriterState(value.writer_state) &&
		[
			'last_frame_at_ms',
			'last_write_at_ms',
			'last_finalize_at_ms',
			'last_catalog_commit_at_ms',
			'oldest_retained_at_ms',
			'newest_retained_at_ms',
			'effective_retention_ms'
		].every((key) => isNullableNumber(value[key])) &&
		[
			'recording_bytes',
			'estimated_bytes_per_day',
			'selected_coverage_ms',
			'coverage_percent',
			'gap_count',
			'largest_gap_ms',
			'playable_fragments',
			'range_count'
		].every((key) => isFiniteNumber(value[key])) &&
		Array.isArray(value.ranges) &&
		value.ranges.length <= 256 &&
		value.ranges.every(
			(range) => isObject(range) && isFiniteNumber(range.start_ms) && isFiniteNumber(range.end_ms)
		) &&
		isFiniteNumber(value.bucket_ms) &&
		Array.isArray(value.buckets) &&
		value.buckets.length <= 256 &&
		value.buckets.every(
			(bucket) =>
				isObject(bucket) &&
				isFiniteNumber(bucket.start_ms) &&
				isFiniteNumber(bucket.end_ms) &&
				isFiniteNumber(bucket.coverage_ms)
		) &&
		typeof value.detail_truncated === 'boolean' &&
		Array.isArray(value.gaps) &&
		value.gaps.length <= 257 &&
		value.gaps.every(isRecordingGap)
	);
}

function isRecordingGap(value: unknown): value is RecordingGap {
	return (
		isObject(value) &&
		isFiniteNumber(value.start_ms) &&
		isNullableNumber(value.end_ms) &&
		isFiniteNumber(value.observed_end_ms) &&
		isFiniteNumber(value.duration_ms) &&
		isGapCause(value.cause) &&
		typeof value.explanation === 'string' &&
		typeof value.evidence_source === 'string' &&
		isNullableString(value.operational_event_id) &&
		isNullableString(value.before_href) &&
		isNullableString(value.after_href) &&
		typeof value.health_href === 'string' &&
		typeof value.logs_href === 'string'
	);
}

function isRecordingFinding(value: unknown): boolean {
	return (
		isObject(value) &&
		typeof value.severity === 'string' &&
		typeof value.camera_id === 'string' &&
		typeof value.camera_name === 'string' &&
		isNullableString(value.stream_id) &&
		typeof value.kind === 'string' &&
		typeof value.message === 'string' &&
		isNullableNumber(value.started_at_ms) &&
		typeof value.health_href === 'string' &&
		isNullableString(value.playback_href) &&
		typeof value.logs_href === 'string'
	);
}

function isCoverageState(value: unknown): boolean {
	return (
		typeof value === 'string' &&
		['healthy', 'degraded', 'paused_by_policy', 'not_configured', 'unknown'].includes(value)
	);
}

function isWriterState(value: unknown): boolean {
	return (
		typeof value === 'string' &&
		['progressing', 'stalled', 'failed', 'pending', 'policy_disabled', 'unknown'].includes(value)
	);
}

function isGapCause(value: unknown): boolean {
	return (
		typeof value === 'string' &&
		[
			'source_silence',
			'transport_outage',
			'stale_frames',
			'decode_failure',
			'writer_failure',
			'disk_pressure',
			'retention_deletion',
			'migration',
			'catalog_mismatch',
			'unknown'
		].includes(value)
	);
}

function isObject(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object';
}

function isFiniteNumber(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value);
}

function isNullableNumber(value: unknown): value is number | null {
	return value === null || isFiniteNumber(value);
}

function isNullableString(value: unknown): value is string | null {
	return value === null || typeof value === 'string';
}
