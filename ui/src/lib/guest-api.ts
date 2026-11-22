export type GuestHandoff =
	{ kind: 'none' } | { kind: 'invalid' } | { kind: 'token'; token: string };
export type GuestAccessStatus = 'expired' | 'revoked' | 'unavailable' | 'limit_reached';
export type GuestReady = {
	status: 'ready';
	session_id: string;
	display_name: string;
	start_ms: number;
	end_ms: number;
	expires_at_ms: number;
	can_stream: boolean;
	can_download: boolean;
	bytes: number;
	has_gaps: boolean;
};
export type GuestSession = GuestReady | { status: GuestAccessStatus; session_id?: string };

declare global {
	interface Window {
		__keeppeekTakeGuestToken?: () => GuestHandoff;
	}
}

const unavailable = (): GuestSession => ({ status: 'unavailable' });
const tokenPattern = /^[A-Za-z0-9_-]{43}$/;
const sessionPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const invalidResponse = 'Shared review response is invalid.';
const requestFailed = 'The shared review could not be reached. Try again.';
const terminalStatuses = new Set<GuestAccessStatus>([
	'expired',
	'revoked',
	'unavailable',
	'limit_reached'
]);

export function consumeGuestHandoff(): GuestHandoff {
	if (typeof window === 'undefined') return { kind: 'invalid' };
	const take = window.__keeppeekTakeGuestToken;
	delete window.__keeppeekTakeGuestToken;
	if (!take) return { kind: 'invalid' };
	try {
		const value = take();
		if (value.kind === 'none' || value.kind === 'invalid') return value;
		if (value.kind === 'token' && tokenPattern.test(value.token)) return value;
	} catch {
		return { kind: 'invalid' };
	}
	return { kind: 'invalid' };
}

function sessionPath(path: string, sessionId?: string): string {
	if (sessionId === undefined) return path;
	if (!sessionPattern.test(sessionId)) throw new Error('Shared review session is invalid.');
	return `${path}?session=${encodeURIComponent(sessionId)}`;
}

export function guestMediaUrl(sessionId: string): string {
	return sessionPath('/guest/media', sessionId);
}

export function guestDownloadUrl(sessionId: string): string {
	return sessionPath('/guest/download', sessionId);
}

export async function exchangeGuestSession(
	token: string,
	signal?: AbortSignal
): Promise<GuestSession> {
	if (!tokenPattern.test(token)) throw new Error('Shared review link is invalid.');
	const result = await requestStatus('POST', undefined, signal, token);
	return result.status === 'ready' ? result : unavailable();
}

export function getGuestSession(sessionId?: string, signal?: AbortSignal): Promise<GuestSession> {
	return requestStatus('GET', sessionId, signal);
}

export async function endGuestSession(sessionId: string, signal?: AbortSignal): Promise<void> {
	const response = await fetch(sessionPath('/guest/session', sessionId), {
		method: 'DELETE',
		credentials: 'include',
		cache: 'no-store',
		redirect: 'error',
		signal: requestSignal(signal),
		headers: { Accept: 'application/json' }
	});
	if (!response.ok) throw new Error(requestFailed);
}

function requestSignal(signal?: AbortSignal): AbortSignal {
	const deadline = AbortSignal.timeout(10_000);
	return signal ? AbortSignal.any([signal, deadline]) : deadline;
}

async function requestStatus(
	method: 'GET' | 'POST',
	sessionId?: string,
	signal?: AbortSignal,
	token?: string
): Promise<GuestSession> {
	let response: Response;
	try {
		response = await fetch(sessionPath('/guest/session', sessionId), {
			method,
			credentials: 'include',
			cache: 'no-store',
			redirect: 'error',
			signal: requestSignal(signal),
			headers:
				token === undefined
					? { Accept: 'application/json' }
					: {
							Accept: 'application/json',
							'Content-Type': 'application/json'
						},
			...(token === undefined ? {} : { body: JSON.stringify({ token }) })
		});
	} catch (failure) {
		if (signal?.aborted) throw failure;
		throw new Error(requestFailed);
	}
	if ([401, 403, 404, 410].includes(response.status)) return unavailable();
	if (!response.ok) throw new Error(requestFailed);
	return parseSession(await readStatusBody(response), sessionId);
}

async function readStatusBody(response: Response): Promise<unknown> {
	const reader = response.body?.getReader();
	if (!reader) throw new Error(invalidResponse);
	const decoder = new TextDecoder('utf-8', { fatal: true });
	let bytes = 0;
	let text = '';
	try {
		for (;;) {
			const next = await reader.read();
			if (next.done) break;
			bytes += next.value.byteLength;
			if (bytes > 4_096) throw new Error(invalidResponse);
			text += decoder.decode(next.value, { stream: true });
		}
		return JSON.parse(text + decoder.decode());
	} catch {
		await reader.cancel().catch(() => undefined);
		throw new Error(invalidResponse);
	} finally {
		reader.releaseLock();
	}
}

function safeInteger(value: unknown): value is number {
	return (
		typeof value === 'number' &&
		Number.isSafeInteger(value) &&
		value >= 0 &&
		value <= 8_640_000_000_000_000
	);
}

function parseSession(value: unknown, boundSession?: string): GuestSession {
	if (value === null || typeof value !== 'object' || Array.isArray(value))
		throw new Error(invalidResponse);
	const data = value as Record<string, unknown>;
	if (data.status === 'unavailable') return unavailable();
	if (typeof data.session_id !== 'string' || !sessionPattern.test(data.session_id))
		throw new Error(invalidResponse);
	if (boundSession !== undefined && data.session_id !== boundSession) return unavailable();
	if (typeof data.status === 'string' && terminalStatuses.has(data.status as GuestAccessStatus)) {
		return { status: data.status as GuestAccessStatus, session_id: data.session_id };
	}
	if (data.status !== 'ready') throw new Error(invalidResponse);
	if (
		typeof data.display_name !== 'string' ||
		new TextEncoder().encode(data.display_name).byteLength > 128 ||
		!safeInteger(data.start_ms) ||
		!safeInteger(data.end_ms) ||
		data.end_ms <= data.start_ms ||
		data.end_ms - data.start_ms > 120_000 ||
		!safeInteger(data.expires_at_ms) ||
		!safeInteger(data.bytes) ||
		data.bytes === 0 ||
		data.bytes > 536_870_912 ||
		typeof data.can_stream !== 'boolean' ||
		typeof data.can_download !== 'boolean' ||
		(!data.can_stream && !data.can_download) ||
		typeof data.has_gaps !== 'boolean'
	)
		throw new Error(invalidResponse);
	return {
		status: 'ready',
		session_id: data.session_id,
		display_name: data.display_name,
		start_ms: data.start_ms,
		end_ms: data.end_ms,
		expires_at_ms: data.expires_at_ms,
		can_stream: data.can_stream,
		can_download: data.can_download,
		bytes: data.bytes,
		has_gaps: data.has_gaps
	};
}
