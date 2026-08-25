import { afterEach, describe, expect, it, vi } from 'vitest';
import { fetchLogSnapshot, fetchMetricsSnapshot, waitForMetricsAt } from './api';

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
