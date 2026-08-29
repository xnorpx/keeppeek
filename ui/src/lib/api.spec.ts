import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	fetchLogSnapshot,
	fetchMetricsSnapshot,
	fetchRecordingCoverage,
	waitForMetricsAt
} from './api';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('KeepPeek API client', () => {
	it('fetches and validates the complete retained log snapshot without caching', async () => {
		const snapshot = {
			entries: [
				{
					sequence: 1,
					timestamp_ms: 2,
					level: 'info',
					target: 'keeppeek::test',
					message: 'ready',
					fields: {}
				}
			],
			oldest_sequence: 1,
			newest_sequence: 1,
			truncated: false,
			stats: {
				entry_count: 1,
				byte_count: 32,
				evicted_entries: 0,
				max_entries: 10_000,
				max_bytes: 8_388_608,
				active_streams: 0,
				max_streams: 8
			}
		};
		const fetchMock = vi.fn(async () => Response.json(snapshot));
		vi.stubGlobal('fetch', fetchMock);

		await expect(fetchLogSnapshot()).resolves.toEqual(snapshot);
		expect(fetchMock).toHaveBeenCalledWith('/logs/snapshot', {
			headers: { Accept: 'application/json' },
			cache: 'no-store'
		});
	});

	it('fetches a no-cache Prometheus snapshot', async () => {
		const fetchMock = vi.fn(async () => new Response('keeppeek_server_info 1\n'));
		vi.stubGlobal('fetch', fetchMock);

		await expect(fetchMetricsSnapshot()).resolves.toBe('keeppeek_server_info 1\n');
		expect(fetchMock).toHaveBeenCalledWith('/metrics', {
			headers: { Accept: 'text/plain' },
			cache: 'no-store'
		});
	});

	it('sends an in-memory bearer without reflecting response bodies into errors', async () => {
		const accessKey = '550e8400-e29b-41d4-a716-446655440000';
		const fetchMock = vi.fn(async () => new Response(accessKey, { status: 401 }));
		vi.stubGlobal('fetch', fetchMock);

		const failure = await fetchMetricsSnapshot(accessKey).catch((error: unknown) => error);

		expect(fetchMock).toHaveBeenCalledWith('/metrics', {
			headers: { Accept: 'text/plain', Authorization: `Bearer ${accessKey}` },
			cache: 'no-store'
		});
		expect(String(failure)).not.toContain(accessKey);
	});

	it('fetches and validates a bounded recording coverage page', async () => {
		const snapshot = recordingCoverageSnapshot();
		const fetchMock = vi.fn(async () => Response.json(snapshot));
		vi.stubGlobal('fetch', fetchMock);

		await expect(
			fetchRecordingCoverage(
				{
					startMs: 1_000,
					endMs: 2_000,
					minimumGapMs: 100,
					minimumCameraGapMs: 500,
					pageSize: 25,
					search: 'Front Door',
					state: 'degraded',
					stream: 'main',
					group: 'Exterior'
				},
				'access-key'
			)
		).resolves.toEqual(snapshot);
		expect(fetchMock).toHaveBeenCalledWith(
			'/recording-coverage?start_ms=1000&end_ms=2000&minimum_gap_ms=100&minimum_camera_gap_ms=500&page_size=25&search=Front+Door&state=degraded&stream=main&group=Exterior',
			{
				headers: { Accept: 'application/json', Authorization: 'Bearer access-key' },
				cache: 'no-store'
			}
		);
	});

	it('rejects malformed recording coverage evidence', async () => {
		const snapshot = recordingCoverageSnapshot();
		const fetchMock = vi.fn(async () =>
			Response.json({
				...snapshot,
				cameras: [
					{
						...snapshot.cameras[0],
						streams: [{ ...snapshot.cameras[0].streams[0], last_write_at_ms: 'recently' }]
					}
				]
			})
		);
		vi.stubGlobal('fetch', fetchMock);

		await expect(fetchRecordingCoverage()).rejects.toThrow(
			'Server returned an invalid recording coverage snapshot.'
		);
	});

	it('checks canonical metrics at a changed server origin without requiring CORS', async () => {
		const fetchMock = vi.fn(async () => new Response(null, { status: 200 }));
		vi.stubGlobal('window', { location: { origin: 'http://127.0.0.1:4174' } });
		vi.stubGlobal('fetch', fetchMock);

		await expect(waitForMetricsAt('http://127.0.0.1:3200')).resolves.toBeUndefined();

		expect(fetchMock).toHaveBeenCalledWith(new URL('http://127.0.0.1:3200/metrics'), {
			mode: 'no-cors'
		});
	});
});

function recordingCoverageSnapshot() {
	return {
		generated_at_ms: 2_000,
		catalog_available: true,
		catalog_revision: 7,
		catalog_updated_at_ms: 1_900,
		window: { start_ms: 1_000, end_ms: 2_000, minimum_gap_ms: 100 },
		totals: {
			cameras: 1,
			healthy: 0,
			degraded: 1,
			paused_by_policy: 0,
			not_configured: 0,
			unknown: 0,
			recording_bytes: 1_024,
			estimated_bytes_per_day: 8_192
		},
		storage: {
			pressure: 'normal',
			recording_state: 'active',
			available_bytes: 4_096,
			effective_limit_bytes: 8_192,
			recording_bytes: 1_024,
			estimated_bytes_per_day: 8_192,
			projected_retention_days: 1,
			projection_assumption: 'Selected bytes scaled to one day'
		},
		groups: ['Exterior'],
		cameras: [
			{
				camera_id: 'front-door',
				camera_name: 'Front Door',
				groups: ['Exterior'],
				state: 'degraded',
				recording_requested: true,
				policy: 'main',
				health_href: '/system-health/camera/front-door',
				streams: [
					{
						stream_id: 'main',
						recording_stream_id: 'front-door/main',
						recording_requested: true,
						writer_state: 'stalled',
						last_frame_at_ms: 1_950,
						last_write_at_ms: 1_700,
						last_finalize_at_ms: 1_600,
						last_catalog_commit_at_ms: 1_900,
						oldest_retained_at_ms: 500,
						newest_retained_at_ms: 1_800,
						effective_retention_ms: 1_300,
						recording_bytes: 1_024,
						estimated_bytes_per_day: 8_192,
						selected_coverage_ms: 800,
						coverage_percent: 80,
						gap_count: 1,
						largest_gap_ms: 200,
						playable_fragments: 4,
						ranges: [{ start_ms: 1_000, end_ms: 1_800 }],
						range_count: 1,
						bucket_ms: 900_000,
						buckets: [{ start_ms: 1_000, end_ms: 2_000, coverage_ms: 800 }],
						detail_truncated: false,
						gaps: [
							{
								start_ms: 1_800,
								end_ms: null,
								observed_end_ms: 2_000,
								duration_ms: 200,
								cause: 'writer_failure',
								explanation: 'Writer is not progressing',
								evidence_source: 'recording_writer',
								operational_event_id: null,
								before_href: '/keep?camera=front-door&stream=main&at=1799',
								after_href: null,
								health_href: '/system-health/camera/front-door',
								logs_href: '/settings/logs'
							}
						]
					}
				]
			}
		],
		findings: [
			{
				severity: 'warning',
				camera_id: 'front-door',
				camera_name: 'Front Door',
				stream_id: 'main',
				kind: 'writer_state',
				message: 'Recording writer is not progressing',
				started_at_ms: 1_700,
				health_href: '/system-health/camera/front-door',
				playback_href: null,
				logs_href: '/settings/logs'
			}
		],
		next_page_token: null
	} as const;
}
