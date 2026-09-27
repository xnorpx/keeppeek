/// <reference types="svelte" />
import { ApiRequestError } from './api';

export type BrowserCredential = { csrfToken: string };
export type AuthenticationMethod = { id: string; name: string; kind: 'oidc' | 'proxy' };
export type BrowserSession = {
	local: boolean;
	bearer_enabled: boolean;
	methods: AuthenticationMethod[];
	identity: {
		id: string;
		display_name: string;
		role: 'administrator' | 'user';
		provider_id?: string;
	} | null;
	csrf_token: string | null;
};
export type AdministratorVerificationStart = {
	origin: string;
	provider_id: string;
	csrf_token: string;
	candidate_plan_id: string;
};

export class ExternalAuthentication {
	session = $state.raw<BrowserSession | null>(null);
	busy = $state(false);
	error = $state<string | null>(null);
	#discovery: Promise<void> | null = null;

	get credential(): BrowserCredential | null {
		const session = this.session;
		return session && !session.local && session.identity && session.csrf_token
			? { csrfToken: session.csrf_token }
			: null;
	}

	discover(): Promise<void> {
		if (this.#discovery) return this.#discovery;
		this.#discovery = this.loadSession().finally(() => {
			this.#discovery = null;
		});
		return this.#discovery;
	}

	private async loadSession(): Promise<void> {
		this.busy = true;
		this.error = null;
		try {
			const response = await fetch('/auth/session', {
				credentials: 'same-origin',
				cache: 'no-store',
				redirect: 'error',
				headers: { Accept: 'application/json' },
				signal: AbortSignal.timeout(10_000)
			});
			if (!response.ok) throw new ApiRequestError(response.status, 'Sign-in is unavailable.');
			const value: unknown = await response.json();
			if (!isBrowserSession(value)) throw new Error('Invalid sign-in response.');
			this.session = value;
		} catch (error) {
			this.session = null;
			this.error = authenticationError(error);
			throw error;
		} finally {
			this.busy = false;
		}
	}

	async logout(): Promise<void> {
		this.error = null;
		const csrf = this.session?.csrf_token;
		if (!csrf) throw new Error('Refresh sign-in before signing out.');
		const response = await fetch('/auth/logout', {
			method: 'POST',
			credentials: 'same-origin',
			cache: 'no-store',
			redirect: 'error',
			headers: { 'X-KeepPeek-CSRF': csrf },
			signal: AbortSignal.timeout(10_000)
		});
		if (!response.ok) throw new ApiRequestError(response.status, 'Sign-out failed.');
		this.session = null;
	}
}

export function authenticationError(error: unknown): string {
	if (error instanceof ApiRequestError) {
		if (error.status === 401) return 'Your session expired or was revoked. Sign in again.';
		if (error.status === 403)
			return 'Access was denied. Ask your administrator to check your account and provider access.';
		if (error.status === 426) return 'Sign-in requires HTTPS or a configured trusted proxy.';
		if (error.status === 429) return 'Too many sign-in attempts. Wait a moment, then retry.';
	}
	return 'KeepPeek or the sign-in provider is unavailable. Retry when the connection is restored.';
}

function record(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object';
}

function boundedString(value: unknown, maximum: number): value is string {
	return typeof value === 'string' && value.length > 0 && value.length <= maximum;
}

export function isBrowserSession(value: unknown): value is BrowserSession {
	if (
		!record(value) ||
		typeof value.local !== 'boolean' ||
		typeof value.bearer_enabled !== 'boolean'
	)
		return false;
	if (!Array.isArray(value.methods) || value.methods.length > 64) return false;
	if (
		!value.methods.every(
			(method) =>
				record(method) &&
				boundedString(method.id, 64) &&
				boundedString(method.name, 256) &&
				['oidc', 'proxy'].includes(String(method.kind))
		)
	)
		return false;
	if (value.csrf_token !== null && !boundedString(value.csrf_token, 128)) return false;
	const identity = value.identity;
	if (identity === null) return true;
	return (
		record(identity) &&
		boundedString(identity.id, 128) &&
		boundedString(identity.display_name, 1024) &&
		['administrator', 'user'].includes(String(identity.role)) &&
		(identity.provider_id === undefined || boundedString(identity.provider_id, 64)) &&
		(value.local || boundedString(value.csrf_token, 128))
	);
}

export function validReturnPath(path: string): boolean {
	return (
		path.length <= 2048 &&
		path.startsWith('/') &&
		!path.startsWith('//') &&
		!path.startsWith('/auth/') &&
		!/[^\x21-\x7e]|[\\%#]/.test(path)
	);
}

export function submitProviderLogin(
	providerId: string,
	csrf: string,
	returnPath: string,
	candidatePlanId?: string
): void {
	if (!boundedString(providerId, 64) || !boundedString(csrf, 128) || !validReturnPath(returnPath))
		throw new Error('Invalid sign-in request.');
	if (candidatePlanId !== undefined && !validPlanId(candidatePlanId))
		throw new Error('Invalid verification plan.');
	// ponytail: Native navigation owns OIDC redirects; no provider token enters application state.
	const form = document.createElement('form');
	form.method = 'POST';
	form.action = '/auth/login';
	const fields: Record<string, string> = {
		provider_id: providerId,
		csrf_token: csrf,
		return_path: returnPath
	};
	if (candidatePlanId) fields.candidate_plan_id = candidatePlanId;
	for (const [name, value] of Object.entries(fields)) {
		const input = document.createElement('input');
		input.type = 'hidden';
		input.name = name;
		input.value = value;
		form.append(input);
	}
	document.body.append(form);
	try {
		form.submit();
	} finally {
		form.remove();
	}
}

function validPlanId(value: unknown): value is string {
	return typeof value === 'string' && /^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value);
}

function isVerificationStart(
	value: unknown,
	origin: string
): value is AdministratorVerificationStart {
	return (
		record(value) &&
		value.origin === origin &&
		boundedString(value.provider_id, 64) &&
		boundedString(value.csrf_token, 128) &&
		validPlanId(value.candidate_plan_id)
	);
}

export function verificationStartFromMessage(
	event: Pick<MessageEvent, 'origin' | 'source' | 'data'>,
	opener: Window | null,
	origin: string,
	openerOrigin = origin
): AdministratorVerificationStart | null {
	if (!opener || event.source !== opener || event.origin !== openerOrigin) return null;
	const data: unknown = event.data;
	return record(data) &&
		data.type === 'keeppeek-verification-start' &&
		isVerificationStart(data.start, origin)
		? data.start
		: null;
}

/** The opener retains its control session. Only the server-issued start crosses this channel. */
export function openAdministratorVerification(start: AdministratorVerificationStart): () => void {
	if (
		!canonicalOrigin(start.origin) ||
		(start.origin !== window.location.origin && !start.origin.startsWith('https://')) ||
		!isVerificationStart(start, start.origin)
	)
		throw new Error('Verification requires an exact HTTPS recorder origin.');
	const payload: AdministratorVerificationStart = {
		origin: start.origin,
		provider_id: start.provider_id,
		csrf_token: start.csrf_token,
		candidate_plan_id: start.candidate_plan_id
	};
	// The fragment carries only the public parent origin. The start challenge stays out of URLs.
	const destination = `${payload.origin}/#verify-administrator?opener=${encodeURIComponent(window.location.origin)}`;
	const popup = window.open(destination, '_blank', 'popup,width=520,height=680');
	if (!popup) throw new Error('Allow pop-ups to verify administrator access.');
	const receive = (event: MessageEvent) => {
		if (event.origin !== payload.origin || event.source !== popup) return;
		if (record(event.data) && event.data.type === 'keeppeek-verification-ready') {
			popup.postMessage({ type: 'keeppeek-verification-start', start: payload }, payload.origin);
			cleanup();
		}
	};
	const cleanup = () => {
		window.removeEventListener('message', receive);
		clearTimeout(timer);
	};
	const timer = setTimeout(cleanup, 60_000);
	window.addEventListener('message', receive);
	return cleanup;
}

export function isAdministratorVerificationWindow(url: URL): boolean {
	return (
		url.hash.split('?')[0] === '#verify-administrator' ||
		url.searchParams.get('verification') === 'complete'
	);
}

function canonicalOrigin(value: string): boolean {
	if (!boundedString(value, 2048)) return false;
	try {
		const url = new URL(value);
		return (url.protocol === 'https:' || url.protocol === 'http:') && url.origin === value;
	} catch {
		return false;
	}
}

export function verificationOpenerOrigin(url: URL): string | null {
	if (url.hash === '#verify-administrator') return url.origin;
	const marker = '#verify-administrator?';
	if (!url.hash.startsWith(marker)) return null;
	const fields = new URLSearchParams(url.hash.slice(marker.length));
	const origin = fields.get('opener');
	return fields.size === 1 && origin && canonicalOrigin(origin) ? origin : null;
}
