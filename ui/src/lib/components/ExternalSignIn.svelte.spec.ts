import { mount, tick, unmount } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import ExternalSignIn from './ExternalSignIn.svelte';
import AdministratorVerificationWindow from './AdministratorVerificationWindow.svelte';
import {
	ExternalAuthentication,
	isAdministratorVerificationWindow
} from '$lib/external-authentication.svelte';

const components: Array<ReturnType<typeof mount>> = [];
const targets: HTMLElement[] = [];
const originalUrl = location.href;
const start = {
	origin: location.origin,
	provider_id: 'company',
	csrf_token: 'csrf',
	candidate_plan_id: '12345678-1234-1234-1234-123456789abc'
};

function target(): HTMLElement {
	const element = document.createElement('div');
	document.body.append(element);
	targets.push(element);
	return element;
}

function deliver(origin = location.origin, source: Window | null = window): void {
	window.dispatchEvent(
		new MessageEvent('message', {
			origin,
			source,
			data: { type: 'keeppeek-verification-start', start }
		})
	);
}

function prepareVerificationWindow(openerOrigin = location.origin): void {
	const url = new URL(location.href);
	url.hash = `verify-administrator?opener=${encodeURIComponent(openerOrigin)}`;
	history.replaceState(null, '', url);
	vi.stubGlobal('opener', window);
}

afterEach(async () => {
	for (const component of components.splice(0)) await unmount(component);
	for (const element of targets.splice(0)) element.remove();
	history.replaceState(null, '', originalUrl);
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
	vi.useRealTimers();
});

describe('sign-in and administrator verification UI', () => {
	it('renders provider sign-in and announces discovery outages without exposing server responses', async () => {
		const auth = new ExternalAuthentication();
		auth.session = {
			local: false,
			bearer_enabled: false,
			identity: null,
			csrf_token: 'csrf',
			methods: [{ id: 'company', name: 'Company', kind: 'oidc' }]
		};
		const request = vi
			.spyOn(globalThis, 'fetch')
			.mockResolvedValue(new Response('private provider detail', { status: 503 }));
		components.push(
			mount(ExternalSignIn, {
				target: target(),
				props: {
					authentication: auth,
					state: { status: 'sign-in-required', session: null, message: null, generation: 0 },
					onsignin: vi.fn(),
					onretry: vi.fn()
				}
			})
		);
		await page.getByRole('button', { name: 'Continue with Company' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent(
				'KeepPeek or the sign-in provider is unavailable. Retry when the connection is restored.'
			);
		expect(document.body.textContent).not.toContain('private provider detail');
		expect(request).toHaveBeenCalledOnce();
	});
	it('accepts one opener-bound start, submits only the existing login form, and never discovers a session', async () => {
		prepareVerificationWindow();
		const post = vi.spyOn(window, 'postMessage').mockImplementation(() => {});
		const request = vi.spyOn(globalThis, 'fetch');
		let fields: Record<string, FormDataEntryValue> = {};
		const submit = vi.spyOn(HTMLFormElement.prototype, 'submit').mockImplementation(function (
			this: HTMLFormElement
		) {
			fields = Object.fromEntries(new FormData(this));
			expect(this.action).toBe(`${location.origin}/auth/login`);
			expect(this.method).toBe('post');
		});
		components.push(mount(AdministratorVerificationWindow, { target: target() }));
		await tick();
		expect(post).toHaveBeenCalledExactlyOnceWith(
			{ type: 'keeppeek-verification-ready' },
			location.origin
		);
		deliver('https://evil.test');
		deliver(location.origin, null);
		expect(submit).not.toHaveBeenCalled();
		deliver();
		deliver();
		expect(submit).toHaveBeenCalledOnce();
		expect(fields).toEqual({
			provider_id: 'company',
			csrf_token: 'csrf',
			candidate_plan_id: start.candidate_plan_id,
			return_path: '/?verification=complete'
		});
		expect(request).not.toHaveBeenCalled();
	});
	it('removes the child listener on unmount', async () => {
		prepareVerificationWindow();
		const post = vi.spyOn(window, 'postMessage').mockImplementation(() => {});
		const submit = vi.spyOn(HTMLFormElement.prototype, 'submit').mockImplementation(() => {});
		const component = mount(AdministratorVerificationWindow, { target: target() });
		await tick();
		expect(post).toHaveBeenCalledOnce();
		await unmount(component);
		deliver();
		expect(submit).not.toHaveBeenCalled();
	});
	it('expires a waiting request and rejects late messages', async () => {
		vi.useFakeTimers();
		prepareVerificationWindow();
		vi.spyOn(window, 'postMessage').mockImplementation(() => {});
		const submit = vi.spyOn(HTMLFormElement.prototype, 'submit').mockImplementation(() => {});
		components.push(mount(AdministratorVerificationWindow, { target: target() }));
		await tick();
		vi.advanceTimersByTime(60_000);
		await tick();
		deliver();
		expect(submit).not.toHaveBeenCalled();
		expect(document.querySelector('[role="alert"]')?.textContent).toContain('expired');
	});
	it('binds a different-origin opener without submitting to that parent origin', async () => {
		const parentOrigin = 'https://original-recorder.example';
		prepareVerificationWindow(parentOrigin);
		const post = vi.spyOn(window, 'postMessage').mockImplementation(() => {});
		const submit = vi.spyOn(HTMLFormElement.prototype, 'submit').mockImplementation(function (
			this: HTMLFormElement
		) {
			expect(this.action).toBe(`${location.origin}/auth/login`);
		});
		components.push(mount(AdministratorVerificationWindow, { target: target() }));
		await tick();
		expect(post).toHaveBeenCalledExactlyOnceWith(
			{ type: 'keeppeek-verification-ready' },
			parentOrigin
		);
		deliver();
		deliver(parentOrigin, null);
		expect(submit).not.toHaveBeenCalled();
		deliver(parentOrigin);
		expect(submit).toHaveBeenCalledOnce();
	});
	it('rejects an opener without an explicit verification destination', async () => {
		vi.stubGlobal('opener', window);
		const post = vi.spyOn(window, 'postMessage').mockImplementation(() => {});
		const submit = vi.spyOn(HTMLFormElement.prototype, 'submit').mockImplementation(() => {});
		components.push(mount(AdministratorVerificationWindow, { target: target() }));
		await tick();
		deliver();
		expect(post).not.toHaveBeenCalled();
		expect(submit).not.toHaveBeenCalled();
		expect(document.querySelector('[role="alert"]')?.textContent).toContain('original KeepPeek');
	});
	it('keeps callback completion isolated from discovery and does not publish a proof receipt', async () => {
		const url = new URL(location.href);
		url.searchParams.set('verification', 'complete');
		history.replaceState(null, '', url);
		const request = vi.spyOn(globalThis, 'fetch');
		const post = vi.spyOn(window, 'postMessage');
		components.push(mount(AdministratorVerificationWindow, { target: target() }));
		await tick();
		expect(isAdministratorVerificationWindow(url)).toBe(true);
		expect(
			isAdministratorVerificationWindow(new URL('/#verify-administrator', location.origin))
		).toBe(true);
		expect(isAdministratorVerificationWindow(new URL('/', location.origin))).toBe(false);
		expect(document.body.textContent).toContain('Return to the original window');
		expect(request).not.toHaveBeenCalled();
		expect(post).not.toHaveBeenCalled();
	});
});
