import { createRawSnippet, tick } from 'svelte';
import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { AccessConnectionState, AccessSession } from '$lib/access';
import AppShell from './AppShell.svelte';

const connection = vi.hoisted(() => ({ emit: (_state: AccessConnectionState) => {} }));
let peekState: import('$lib/peek-view-context.svelte').PeekViewState;
vi.mock('$lib/peek-view-context.svelte', async (original) => {
	const module = await original<typeof import('$lib/peek-view-context.svelte')>();
	return { ...module, setPeekViewState: () => (peekState = module.setPeekViewState()) };
});
vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));
vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/settings') } }));
vi.mock('$app/navigation', () => ({
	goto: vi.fn(),
	afterNavigate: vi.fn(),
	beforeNavigate: vi.fn()
}));
vi.mock('$lib/control-context', () => ({
	setControlClient: () => ({
		onAccessState: (subscribe: typeof connection.emit) => {
			connection.emit = subscribe;
			return () => {};
		},
		onCapabilities: () => () => {},
		checkAccess: async () => {},
		closeOnPageHide: () => {},
		getHealth: async () => null,
		runtimeProfiles: { watchShellProfile: async () => ({ close: async () => {} }) },
		externalAuthentication: { error: null }
	}),
	useControlClient: vi.fn()
}));

function admin(principalId: string): AccessSession {
	return {
		id: 'viewer',
		principalId,
		displayName: principalId,
		role: 'administrator',
		local: true,
		clientClassification: 'local',
		createdAtMs: 1,
		lastActivityAtMs: 2,
		absoluteExpiresAtMs: 100,
		credentialExpiresAtMs: null
	};
}

describe('workspace recovery', () => {
	it('keeps drafts and cached frames on camera grants and discards them on camera loss', async () => {
		const session = admin('viewer');
		const allowed = { session, cameraSourceIds: ['front', 'garage'], generation: 1, message: null };
		const children = createRawSnippet(() => ({
			render: () => '<input aria-label="Camera work" />'
		}));
		const view = await render(AppShell, { props: { children } });
		connection.emit({ ...allowed, cameraSourceIds: ['front'], status: 'authenticated' });
		const input = page.getByRole('textbox', { name: 'Camera work' });
		await input.fill('prior camera detail');
		const previous = input.element();
		peekState.updateCameraFrames({ front: 'retained frame' });
		connection.emit({ ...allowed, status: 'checking', cameraSourceIds: ['front'] });
		await tick();
		connection.emit({ ...allowed, status: 'authenticated' });
		await tick();
		expect(input.element()).toBe(previous);
		await expect.element(input).toHaveValue('prior camera detail');
		expect(peekState.cameraFrame('front')).toBe('retained frame');
		peekState.updateCameraFrames({ garage: 'old cached frame' });
		connection.emit({ ...allowed, status: 'checking' });
		await tick();
		expect(previous.closest('[inert]')).not.toBeNull();
		connection.emit({
			...allowed,
			status: 'authenticated',
			cameraSourceIds: ['front'],
			generation: 2
		});
		await tick();
		expect(peekState.cameraFrame('garage')).toBeNull();
		expect(previous.isConnected).toBe(false);
		await expect.element(input).toHaveValue('');
		expect(input.element().closest('[inert]')).toBeNull();
		await view.unmount();
	});
	it('retains a mounted draft for the same principal and unmounts it on identity change', async () => {
		const children = createRawSnippet(() => ({
			render: () => '<input aria-label="Unsaved setting" />'
		}));
		const session = admin('first');
		const view = await render(AppShell, { props: { children } });
		connection.emit({ status: 'authenticated', session, generation: 1, message: null });
		const input = page.getByRole('textbox', { name: 'Unsaved setting' });
		await input.fill('draft');
		const node = input.element();
		connection.emit({ status: 'checking', session, generation: 2, message: null });
		await tick();
		expect(node.isConnected).toBe(true);
		expect(node.closest('[inert]')).not.toBeNull();
		const pageShortcut = vi.fn();
		window.addEventListener('keydown', pageShortcut);
		try {
			const escape = new KeyboardEvent('keydown', { key: 'Escape', cancelable: true });
			window.dispatchEvent(escape);
			expect(escape.defaultPrevented).toBe(true);
			for (const key of [
				{ key: 'Tab' },
				{ key: 'r', ctrlKey: true },
				{ key: '+', ctrlKey: true }
			]) {
				const event = new KeyboardEvent('keydown', { ...key, cancelable: true });
				window.dispatchEvent(event);
				expect(event.defaultPrevented).toBe(false);
			}
			expect(pageShortcut).not.toHaveBeenCalled();
		} finally {
			window.removeEventListener('keydown', pageShortcut);
		}
		connection.emit({
			status: 'authenticated',
			session: { ...session, id: 'replacement' },
			generation: 3,
			message: null
		});
		await expect.element(input).toHaveValue('draft');
		expect(input.element()).toBe(node);
		connection.emit({
			status: 'authenticated',
			session: admin('second'),
			generation: 4,
			message: null
		});
		await tick();
		expect(node.isConnected).toBe(false);
		await expect.element(input).toHaveValue('');
		await view.unmount();
	});
});
