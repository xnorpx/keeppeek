import type { Page } from '@playwright/test';
import { isRecordingCoverageResponse } from '../../src/lib/api';
import type {
	CameraRecordingCoverage,
	RecordingCoverageResponse,
	RecordingCoverageState
} from '../../src/lib/types';

type QueryState = {
	startMs: number;
	endMs: number;
	minimumGapMs: number;
	minimumCameraGapMs: number;
	pageSize: number;
	search: string;
	state: RecordingCoverageState | '';
	stream: 'main' | 'sub' | '';
	group: string;
};

export async function mockRecordingCoverage(page: Page, cameraCount = 3): Promise<void> {
	const now = Date.now();
	let query: QueryState = {
		startMs: now - 86_400_000,
		endMs: now,
		minimumGapMs: 60_000,
		minimumCameraGapMs: 0,
		pageSize: 25,
		search: '',
		state: '',
		stream: '',
		group: ''
	};
	const cameras = Array.from({ length: cameraCount }, (_, index) => coverageCamera(index, query));

	await page.route(/\/recording-coverage(?:\?.*)?$/, async (route) => {
		const url = new URL(route.request().url());
		const pageToken = url.searchParams.get('page_token');
		if (!pageToken) {
			query = {
				startMs: Number(url.searchParams.get('start_ms') ?? query.startMs),
				endMs: Number(url.searchParams.get('end_ms') ?? query.endMs),
				minimumGapMs: Number(url.searchParams.get('minimum_gap_ms') ?? query.minimumGapMs),
				minimumCameraGapMs: Number(url.searchParams.get('minimum_camera_gap_ms') ?? 0),
				pageSize: Number(url.searchParams.get('page_size') ?? 25),
				search: (url.searchParams.get('search') ?? '').toLowerCase(),
				state: (url.searchParams.get('state') ?? '') as QueryState['state'],
				stream: (url.searchParams.get('stream') ?? '') as QueryState['stream'],
				group: url.searchParams.get('group') ?? ''
			};
		}
		const offset = pageToken ? Number(pageToken) : 0;
		const projected = cameras
			.map((camera, index) => coverageCamera(index, query, camera.camera_id, camera.camera_name))
			.filter(
				(camera) =>
					(!query.search ||
						camera.camera_name.toLowerCase().includes(query.search) ||
						camera.camera_id.toLowerCase().includes(query.search)) &&
					(!query.state || camera.state === query.state) &&
					(!query.stream || camera.streams.some((stream) => stream.stream_id === query.stream)) &&
					(!query.group || camera.groups.includes(query.group)) &&
					(!query.minimumCameraGapMs ||
						camera.streams.some(
							(stream) =>
								stream.recording_requested && stream.largest_gap_ms >= query.minimumCameraGapMs
						))
			);
		const cameraPage = projected.slice(offset, offset + query.pageSize);
		const response = coverageResponse(projected, cameraPage, query, offset);
		if (!isRecordingCoverageResponse(response)) {
			throw new Error('Recording coverage fixture violates the runtime response contract');
		}
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(response)
		});
	});
}

function coverageCamera(
	index: number,
	query: QueryState,
	cameraId?: string,
	cameraName?: string
): CameraRecordingCoverage {
	const id =
		cameraId ?? (index === 0 ? 'front-door' : `camera-${String(index + 1).padStart(3, '0')}`);
	const name =
		cameraName ?? (index === 0 ? 'Front Door' : `Camera ${String(index + 1).padStart(3, '0')}`);
	const state: RecordingCoverageState =
		index === 0
			? 'degraded'
			: index === 2
				? 'paused_by_policy'
				: index === 3
					? 'not_configured'
					: 'healthy';
	const requested = state !== 'paused_by_policy' && state !== 'not_configured';
	const gapMs = state === 'degraded' ? 10 * 60_000 : 0;
	const rangeEnd = query.endMs - gapMs;
	const duration = query.endMs - query.startMs;
	const coverageMs = requested ? duration - gapMs : 0;
	return {
		camera_id: id,
		camera_name: name,
		groups: index % 2 === 0 ? ['Exterior'] : ['Interior'],
		state,
		recording_requested: requested,
		policy: requested ? 'main' : 'off',
		health_href: `/system-health/camera/${id}`,
		streams: [
			{
				stream_id: 'main',
				recording_stream_id: `${id}/main`,
				recording_requested: requested,
				writer_state: requested
					? state === 'degraded'
						? 'stalled'
						: 'progressing'
					: 'policy_disabled',
				last_frame_at_ms: query.endMs - 1_000,
				last_write_at_ms: requested ? rangeEnd : null,
				last_finalize_at_ms: requested ? rangeEnd - 30_000 : null,
				last_catalog_commit_at_ms: query.endMs - 500,
				oldest_retained_at_ms: requested ? query.startMs - 7 * 86_400_000 : null,
				newest_retained_at_ms: requested ? rangeEnd : null,
				effective_retention_ms: requested ? 7 * 86_400_000 : null,
				recording_bytes: requested ? 2_400_000_000 + index * 10_000_000 : 0,
				estimated_bytes_per_day: requested ? 340_000_000 : 0,
				selected_coverage_ms: coverageMs,
				coverage_percent: requested ? (coverageMs / duration) * 100 : 0,
				gap_count: gapMs > 0 ? 1 : 0,
				largest_gap_ms: gapMs,
				playable_fragments: requested ? 1_440 : 0,
				ranges: requested ? [{ start_ms: query.startMs, end_ms: rangeEnd }] : [],
				range_count: requested ? 1 : 0,
				bucket_ms: rangeBucketMs(query.endMs - query.startMs),
				buckets: requested ? coverageBuckets(query.startMs, rangeEnd, query.endMs) : [],
				detail_truncated: false,
				gaps:
					gapMs >= query.minimumGapMs
						? [
								{
									start_ms: rangeEnd,
									end_ms: null,
									observed_end_ms: query.endMs,
									duration_ms: gapMs,
									cause: 'writer_failure',
									explanation: 'Requested recording writes are not progressing',
									evidence_source: 'recording_writer',
									operational_event_id: 'recording-gap-1',
									before_href: `/keep?camera=${id}&stream=main&at=${rangeEnd - 1}`,
									after_href: null,
									health_href: `/system-health/camera/${id}`,
									logs_href: '/settings/logs'
								}
							]
						: []
			}
		]
	};
}

function rangeBucketMs(durationMs: number): number {
	if (durationMs <= 86_400_000) return 15 * 60_000;
	if (durationMs <= 7 * 86_400_000) return 60 * 60_000;
	return 6 * 60 * 60_000;
}

function coverageBuckets(startMs: number, coverageEndMs: number, windowEndMs: number) {
	const bucketMs = rangeBucketMs(windowEndMs - startMs);
	const buckets = [];
	let cursorMs = startMs;
	while (cursorMs < coverageEndMs) {
		const endMs = Math.min(coverageEndMs, cursorMs + bucketMs);
		buckets.push({
			start_ms: cursorMs,
			end_ms: Math.min(windowEndMs, cursorMs + bucketMs),
			coverage_ms: endMs - cursorMs
		});
		cursorMs += bucketMs;
	}
	return buckets;
}

function coverageResponse(
	allCameras: CameraRecordingCoverage[],
	cameras: CameraRecordingCoverage[],
	query: QueryState,
	offset: number
): RecordingCoverageResponse {
	const counts = (state: RecordingCoverageState) =>
		allCameras.filter((camera) => camera.state === state).length;
	const recordingBytes = allCameras.reduce(
		(total, camera) => total + camera.streams[0].recording_bytes,
		0
	);
	const bytesPerDay = allCameras.reduce(
		(total, camera) => total + camera.streams[0].estimated_bytes_per_day,
		0
	);
	return {
		generated_at_ms: query.endMs,
		catalog_available: true,
		catalog_revision: 42,
		catalog_updated_at_ms: query.endMs - 500,
		window: {
			start_ms: query.startMs,
			end_ms: query.endMs,
			minimum_gap_ms: query.minimumGapMs
		},
		totals: {
			cameras: allCameras.length,
			healthy: counts('healthy'),
			degraded: counts('degraded'),
			paused_by_policy: counts('paused_by_policy'),
			not_configured: counts('not_configured'),
			unknown: counts('unknown'),
			recording_bytes: recordingBytes,
			estimated_bytes_per_day: bytesPerDay
		},
		storage: {
			pressure: 'normal',
			recording_state: 'active',
			available_bytes: 800_000_000_000,
			effective_limit_bytes: 1_000_000_000_000,
			recording_bytes: recordingBytes,
			estimated_bytes_per_day: bytesPerDay,
			projected_retention_days: bytesPerDay > 0 ? 1_000_000_000_000 / bytesPerDay : null,
			projection_assumption: 'Selected finalized playable fragment bytes scaled from 24 hours'
		},
		groups: [...new Set(allCameras.flatMap((camera) => camera.groups))].sort(),
		cameras,
		findings: allCameras
			.filter((camera) => camera.state === 'degraded')
			.map((camera) => ({
				severity: 'warning',
				camera_id: camera.camera_id,
				camera_name: camera.camera_name,
				stream_id: 'main',
				kind: 'writer_state',
				message: 'Recording writer is not progressing',
				started_at_ms: camera.streams[0].last_write_at_ms,
				health_href: camera.health_href,
				playback_href: null,
				logs_href: '/settings/logs'
			})),
		next_page_token:
			offset + query.pageSize < allCameras.length ? String(offset + query.pageSize) : null
	};
}
