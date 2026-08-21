import { afterEach, describe, expect, it, vi } from 'vitest';
import { waitForMetricsAt } from './api';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('KeepPeek API client', () => {
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
