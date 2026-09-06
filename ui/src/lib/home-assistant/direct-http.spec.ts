import { afterEach, describe, expect, it, vi } from 'vitest';
import { createDirectSession, deleteDirectSession } from './direct-http';

const connection = { endpoint: 'https://keeppeek.example.net', token: 'test-only-credential' };
const answer = { session_id: 'test-session', answer: { type: 'answer', sdp: 'v=0\r\n' } };

afterEach(() => vi.unstubAllGlobals());

describe('Home Assistant direct session HTTP', () => {
	it('gzip-encodes the whole offer and accepts the browser-decoded gzip response', async () => {
		const request = vi.fn(async () =>
			Response.json(answer, { status: 201, headers: { 'Content-Encoding': 'gzip' } })
		);
		vi.stubGlobal('fetch', request);
		const offer = { type: 'offer' as const, sdp: 'v=0\r\na=mid:opaque-mid' };
		expect(await createDirectSession(connection, offer)).toEqual(answer);
		const [url, options] = request.mock.calls[0]! as unknown as [string, RequestInit];
		expect(url).toBe(`${connection.endpoint}/create`);
		expect(options.credentials).toBe('omit');
		expect(options.redirect).toBe('error');
		expect(options.headers).toMatchObject({
			Authorization: `Bearer ${connection.token}`,
			'Content-Encoding': 'gzip'
		});
		const decoded = new Blob([options.body as ArrayBuffer])
			.stream()
			.pipeThrough(new DecompressionStream('gzip'));
		expect(await new Response(decoded).json()).toEqual({ offer });
	});

	it.each([401, 403, 400, 500])('does not expose a rejected response body: %i', async (status) => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(connection.token, { status }))
		);
		const result = createDirectSession(connection, { type: 'offer', sdp: 'v=0' });
		await expect(result).rejects.not.toThrow(connection.token);
		await expect(result).rejects.toMatchObject({
			kind: status === 401 || status === 403 ? 'authentication' : 'protocol'
		});
	});

	it('gives actionable CORS/TLS guidance without echoing the underlying error', async () => {
		vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError(connection.token)));
		await expect(createDirectSession(connection, { type: 'offer', sdp: 'v=0' })).rejects.toThrow(
			/allowed_origins/
		);
	});

	it('rejects invalid and oversized successful responses', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => Response.json({ session_id: 42 }, { status: 201 }))
		);
		await expect(createDirectSession(connection, { type: 'offer', sdp: 'v=0' })).rejects.toThrow(
			/response/i
		);
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(' '.repeat(1_048_577), { status: 201 }))
		);
		await expect(createDirectSession(connection, { type: 'offer', sdp: 'v=0' })).rejects.toThrow(
			/response/i
		);
	});

	it('deletes directly with the same authentication and keepalive enabled', async () => {
		const request = vi.fn(async () => new Response(null, { status: 204 }));
		vi.stubGlobal('fetch', request);
		await deleteDirectSession(connection, 'test-session');
		expect(request).toHaveBeenCalledWith(
			`${connection.endpoint}/delete`,
			expect.objectContaining({
				method: 'POST',
				body: JSON.stringify({ session_id: 'test-session' }),
				keepalive: true,
				credentials: 'omit'
			})
		);
	});
});
