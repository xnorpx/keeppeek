import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	ExternalAuthentication,
	openAdministratorVerification,
	submitProviderLogin,
	validReturnPath,
	verificationOpenerOrigin,
	isAdministratorVerificationWindow,
	verificationStartFromMessage,
	type BrowserSession
} from './external-authentication.svelte';
import {
	ApiRequestError,
	applyCookieConfigurationArchive,
	createSession,
	deleteSession,
	fetchMetricsSnapshot
} from './api';

const browserSession: BrowserSession = {
	local: false,
	bearer_enabled: true,
	methods: [{ id: 'company', name: 'Company', kind: 'oidc' }],
	identity: { id: 'user', display_name: 'User', role: 'user', provider_id: 'company' },
	csrf_token: 'csrf'
};

afterEach(() => {
	vi.restoreAllMocks();
	vi.useRealTimers();
});

describe('browser authentication boundaries', () => {
	it('binds a cross-origin child to the exact parent and its own destination', () => {
		const origin = 'https://replacement.example';
		const start = {
			origin,
			provider_id: 'company',
			csrf_token: 'challenge',
			candidate_plan_id: '12345678-1234-1234-1234-123456789abc'
		};
		const event = {
			origin: location.origin,
			source: window,
			data: { type: 'keeppeek-verification-start', start }
		};
		expect(verificationStartFromMessage(event, window, origin, location.origin)).toEqual(start);
		expect(
			verificationStartFromMessage(event, window, 'https://wrong.example', location.origin)
		).toBeNull();
		expect(verificationStartFromMessage(event, window, origin, 'https://wrong.example')).toBeNull();
		expect(verificationStartFromMessage(event, null, origin, location.origin)).toBeNull();
	});
	it('recognizes the verification window without accepting malformed opener origins', () => {
		const child = new URL('https://replacement.example/#verify-administrator');
		for (const origin of [location.origin, 'https://previous.example']) {
			child.hash = `verify-administrator?opener=${encodeURIComponent(origin)}`;
			expect(isAdministratorVerificationWindow(child)).toBe(true);
			expect(verificationOpenerOrigin(child)).toBe(origin);
		}
		for (const origin of [
			'null',
			'*',
			'javascript:alert(1)',
			'https://name:secret@a.test',
			'https://a.test/path',
			'https://a.test/'
		]) {
			child.hash = `verify-administrator?opener=${encodeURIComponent(origin)}`;
			expect(verificationOpenerOrigin(child)).toBeNull();
		}
		child.hash = 'verify-administrator?opener=https://a.test&opener=https://b.test';
		expect(verificationOpenerOrigin(child)).toBeNull();
	});
	it('accepts the server return-path grammar and rejects redirect escapes', () => {
		expect(validReturnPath('/?verification=complete')).toBe(true);
		for (const path of [
			'//evil.test',
			'/%2f',
			'/#verify-administrator',
			'/auth/login',
			'/\\evil',
			'/ a',
			'/é'
		]) {
			expect(validReturnPath(path)).toBe(false);
		}
	});
	it('requires both the exact origin and the opener for verification starts', () => {
		const opener = window;
		const start = {
			origin: location.origin,
			provider_id: 'company',
			csrf_token: 'challenge',
			candidate_plan_id: '12345678-1234-1234-1234-123456789abc'
		};
		const event = {
			origin: location.origin,
			source: opener,
			data: { type: 'keeppeek-verification-start', start }
		};
		expect(verificationStartFromMessage(event, opener, location.origin)).toEqual(start);
		expect(
			verificationStartFromMessage({ ...event, source: null }, opener, location.origin)
		).toBeNull();
		expect(
			verificationStartFromMessage(
				{ ...event, origin: 'https://evil.test' },
				opener,
				location.origin
			)
		).toBeNull();
		expect(verificationStartFromMessage(event, null, location.origin)).toBeNull();
	});
	it('discovers cookie authentication without a bearer and keeps its CSRF only in memory', async () => {
		const fetcher = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
			Response.json({
				local: false,
				bearer_enabled: false,
				methods: [{ id: 'company', name: 'Company', kind: 'oidc' }],
				identity: { id: 'user', display_name: 'User', role: 'user', provider_id: 'company' },
				csrf_token: 'csrf'
			})
		);
		try {
			const auth = new ExternalAuthentication();
			await auth.discover();
			expect(auth.credential).toEqual({ csrfToken: 'csrf' });
			expect(fetcher).toHaveBeenCalledWith(
				'/auth/session',
				expect.objectContaining({ credentials: 'same-origin', cache: 'no-store' })
			);
			expect(new Headers(fetcher.mock.calls[0][1]?.headers).has('Authorization')).toBe(false);
		} finally {
			fetcher.mockRestore();
		}
	});
	it('adds CSRF to every cookie mutation including teardown and configuration restore', async () => {
		const request = vi
			.spyOn(globalThis, 'fetch')
			.mockResolvedValueOnce(
				Response.json({ session_id: 'session', answer: { type: 'answer', sdp: 'answer' } })
			)
			.mockResolvedValueOnce(new Response(null, { status: 204 }))
			.mockResolvedValueOnce(
				Response.json({ restoreId: 'restore', state: 'RESTORE_STATE_AWAITING_RESTART' })
			);
		const credential = { csrfToken: 'csrf' };
		await createSession({ type: 'offer', sdp: 'offer' }, credential);
		await deleteSession('session', credential, { keepalive: true });
		await applyCookieConfigurationArchive(new File(['zip'], 'backup.zip'), credential);
		expect(request.mock.calls.map(([path]) => path)).toEqual([
			'/create',
			'/delete',
			'/config/apply'
		]);
		for (const [, init] of request.mock.calls) {
			expect(init?.credentials).toBe('same-origin');
			expect(init?.method).toBe('POST');
			expect(new Headers(init?.headers).get('X-KeepPeek-CSRF')).toBe('csrf');
			expect(new Headers(init?.headers).has('Authorization')).toBe(false);
		}
		expect(request.mock.calls[1][1]?.keepalive).toBe(true);
	});
	it('uses cookies for reads and preserves explicitly selected legacy bearer authentication', async () => {
		const request = vi
			.spyOn(globalThis, 'fetch')
			.mockImplementation(async () => new Response('metrics'));
		await fetchMetricsSnapshot({ csrfToken: 'csrf' });
		await fetchMetricsSnapshot('legacy-key');
		expect(new Headers(request.mock.calls[0][1]?.headers).has('Authorization')).toBe(false);
		expect(request.mock.calls[0][1]?.credentials).toBe('same-origin');
		expect(new Headers(request.mock.calls[1][1]?.headers).get('Authorization')).toBe(
			'Bearer legacy-key'
		);
	});
	it('retains the cookie session on logout failure and clears it only after successful logout', async () => {
		const request = vi
			.spyOn(globalThis, 'fetch')
			.mockResolvedValueOnce(new Response(null, { status: 503 }))
			.mockResolvedValueOnce(new Response(null, { status: 204 }));
		const auth = new ExternalAuthentication();
		auth.session = browserSession;
		await expect(auth.logout()).rejects.toBeInstanceOf(ApiRequestError);
		expect(auth.credential).toEqual({ csrfToken: 'csrf' });
		auth.error = 'Previous sign-out failed.';
		await auth.logout();
		expect(auth.session).toBeNull();
		expect(auth.error).toBeNull();
		for (const [path, init] of request.mock.calls) {
			expect(path).toBe('/auth/logout');
			expect(init?.credentials).toBe('same-origin');
			expect(new Headers(init?.headers).get('X-KeepPeek-CSRF')).toBe('csrf');
			expect(new Headers(init?.headers).has('Authorization')).toBe(false);
		}
	});
	it('coalesces discovery and clears stale identity when access is denied', async () => {
		const request = vi
			.spyOn(globalThis, 'fetch')
			.mockResolvedValue(new Response(null, { status: 403 }));
		const auth = new ExternalAuthentication();
		auth.session = browserSession;
		const first = auth.discover();
		expect(auth.discover()).toBe(first);
		await expect(first).rejects.toBeInstanceOf(ApiRequestError);
		expect(request).toHaveBeenCalledOnce();
		expect(auth.session).toBeNull();
		expect(auth.error).toContain('Access was denied');
		expect(auth.busy).toBe(false);
	});
	it('submits only approved login fields by POST without retaining the temporary form', () => {
		let submitted: Record<string, FormDataEntryValue> = {};
		const submit = vi.spyOn(HTMLFormElement.prototype, 'submit').mockImplementation(function (
			this: HTMLFormElement
		) {
			expect(this.method).toBe('post');
			expect(this.action).toBe(`${location.origin}/auth/login`);
			submitted = Object.fromEntries(new FormData(this));
		});
		const storage = vi.spyOn(Storage.prototype, 'setItem');
		submitProviderLogin('company', 'csrf', '/events');
		expect(submitted).toEqual({
			provider_id: 'company',
			csrf_token: 'csrf',
			return_path: '/events'
		});
		expect(document.querySelector('form[action="/auth/login"]')).toBeNull();
		expect(storage).not.toHaveBeenCalled();
		expect(() => submitProviderLogin('company', 'csrf', '//evil.test')).toThrow();
		expect(submit).toHaveBeenCalledOnce();
	});
	it('sends only the four start fields to the exact popup once and removes its listener', () => {
		const popup = window;
		vi.spyOn(window, 'open').mockReturnValue(popup);
		const post = vi.spyOn(popup, 'postMessage').mockImplementation(() => {});
		const start = {
			origin: location.origin,
			provider_id: 'company',
			csrf_token: 'csrf',
			candidate_plan_id: '12345678-1234-1234-1234-123456789abc'
		};
		const cleanup = openAdministratorVerification({
			...start,
			bearer: 'never-forward'
		} as typeof start);
		const ready = { type: 'keeppeek-verification-ready' };
		window.dispatchEvent(
			new MessageEvent('message', { origin: 'https://evil.test', source: popup, data: ready })
		);
		window.dispatchEvent(
			new MessageEvent('message', { origin: location.origin, source: null, data: ready })
		);
		expect(post).not.toHaveBeenCalled();
		window.dispatchEvent(
			new MessageEvent('message', { origin: location.origin, source: popup, data: ready })
		);
		window.dispatchEvent(
			new MessageEvent('message', { origin: location.origin, source: popup, data: ready })
		);
		expect(post).toHaveBeenCalledExactlyOnceWith(
			{ type: 'keeppeek-verification-start', start },
			location.origin
		);
		cleanup();
	});
	it('expires and explicitly cancels opener listeners without sending a start', () => {
		vi.useFakeTimers();
		vi.spyOn(window, 'open').mockReturnValue(window);
		const post = vi.spyOn(window, 'postMessage').mockImplementation(() => {});
		const start = {
			origin: location.origin,
			provider_id: 'company',
			csrf_token: 'csrf',
			candidate_plan_id: '12345678-1234-1234-1234-123456789abc'
		};
		openAdministratorVerification(start);
		vi.advanceTimersByTime(60_000);
		const ready = () =>
			window.dispatchEvent(
				new MessageEvent('message', {
					origin: location.origin,
					source: window,
					data: { type: 'keeppeek-verification-ready' }
				})
			);
		ready();
		const cancel = openAdministratorVerification(start);
		cancel();
		ready();
		expect(post).not.toHaveBeenCalled();
		expect(vi.getTimerCount()).toBe(0);
	});
	it('hands a candidate on another HTTPS origin only its one-use start challenge', () => {
		const open = vi.spyOn(window, 'open').mockReturnValue(window);
		const post = vi.spyOn(window, 'postMessage').mockImplementation(() => {});
		const start = {
			origin: 'https://replacement.example',
			provider_id: 'company',
			csrf_token: 'private-start-challenge',
			candidate_plan_id: '12345678-1234-1234-1234-123456789abc'
		};
		const cleanup = openAdministratorVerification(start);
		const destination = new URL(String(open.mock.calls[0][0]));
		expect(destination.origin).toBe(start.origin);
		expect(destination.search).toBe('');
		expect(destination.hash).not.toContain(start.csrf_token);
		expect(destination.hash).not.toContain(start.candidate_plan_id);
		const ready = { type: 'keeppeek-verification-ready' };
		for (const [origin, source] of [
			[location.origin, window],
			[start.origin, null]
		] as const) {
			window.dispatchEvent(new MessageEvent('message', { origin, source, data: ready }));
		}
		expect(post).not.toHaveBeenCalled();
		window.dispatchEvent(
			new MessageEvent('message', {
				origin: start.origin,
				source: window,
				data: ready
			})
		);
		expect(post).toHaveBeenCalledExactlyOnceWith(
			{ type: 'keeppeek-verification-start', start },
			start.origin
		);
		cleanup();
	});
});
