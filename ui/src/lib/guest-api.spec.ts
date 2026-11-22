import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	exchangeGuestSession,
	getGuestSession,
	endGuestSession,
	guestMediaUrl,
	guestDownloadUrl
} from './guest-api';

const session = '76c391c5-4a31-4d58-8510-59c62fa0cf29';
const otherSession = '08a105c4-7ce8-48b8-83d0-8085c0ddc92e';
const token = 'A'.repeat(43);
const ready = {
	status: 'ready',
	session_id: session,
	display_name: 'Front step visit',
	start_ms: 1_000,
	end_ms: 61_000,
	expires_at_ms: 5_000_000,
	can_stream: true,
	can_download: false,
	bytes: 24_600_000,
	has_gaps: false
};

afterEach(() => vi.unstubAllGlobals());

describe('Isolated guest HTTP client', () => {
	it('exchanges only the in-memory token in a same-origin credentialed uncached POST', async () => {
		const fetchMock = vi.fn(async (_url: string, _init?: RequestInit) => Response.json(ready));
		vi.stubGlobal('fetch', fetchMock);
		await expect(exchangeGuestSession(token)).resolves.toEqual(ready);
		expect(fetchMock).toHaveBeenCalledWith(
			'/guest/session',
			expect.objectContaining({
				method: 'POST',
				credentials: 'include',
				cache: 'no-store',
				redirect: 'error',
				body: JSON.stringify({ token }),
				headers: { Accept: 'application/json', 'Content-Type': 'application/json' }
			})
		);
		expect(JSON.stringify(fetchMock.mock.calls[0][0])).not.toContain(token);
	});

	it('resumes cookie context once, then binds status, end and media to its nonsecret session ID', async () => {
		const fetchMock = vi.fn(async (_url: string, init?: RequestInit) =>
			init?.method === 'DELETE' ? new Response(null, { status: 204 }) : Response.json(ready)
		);
		vi.stubGlobal('fetch', fetchMock);
		await expect(getGuestSession()).resolves.toEqual(ready);
		await expect(getGuestSession(session)).resolves.toEqual(ready);
		await endGuestSession(session);
		expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
			'/guest/session',
			`/guest/session?session=${session}`,
			`/guest/session?session=${session}`
		]);
		expect(fetchMock.mock.calls[2][1]).toEqual(
			expect.objectContaining({
				method: 'DELETE',
				credentials: 'include',
				cache: 'no-store'
			})
		);
		expect(guestMediaUrl(session)).toBe(`/guest/media?session=${session}`);
		expect(guestDownloadUrl(session)).toBe(`/guest/download?session=${session}`);
	});

	it('rejects a different tab session without returning its evidence metadata', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, _init?: RequestInit) =>
				Response.json({ ...ready, session_id: otherSession })
			)
		);
		await expect(getGuestSession(session)).resolves.toEqual({ status: 'unavailable' });
	});

	it('keeps all first-access denials generic', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, _init?: RequestInit) =>
				Response.json({ status: 'expired', session_id: session })
			)
		);
		await expect(exchangeGuestSession(token)).resolves.toEqual({ status: 'unavailable' });
	});

	it('never reflects a server error body or rejected token into an error', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, _init?: RequestInit) => new Response(token, { status: 500 }))
		);
		const failure = await exchangeGuestSession(token).catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(Error);
		expect(String(failure)).not.toContain(token);
	});

	it.each([
		{ end_ms: 1_000 },
		{ end_ms: 121_001 },
		{ bytes: 536_870_913 },
		{ session_id: '../another-artifact' },
		{ can_stream: 'yes' },
		{ has_gaps: null }
	])('rejects malformed ready metadata %j', async (change) => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, _init?: RequestInit) => Response.json({ ...ready, ...change }))
		);
		await expect(getGuestSession()).rejects.toThrow('Shared review response is invalid.');
	});

	it('rejects malformed token/session inputs before any request', async () => {
		const fetchMock = vi.fn();
		vi.stubGlobal('fetch', fetchMock);
		await expect(exchangeGuestSession('bad')).rejects.toThrow();
		await expect(getGuestSession('bad')).rejects.toThrow();
		expect(() => guestMediaUrl('bad')).toThrow();
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
