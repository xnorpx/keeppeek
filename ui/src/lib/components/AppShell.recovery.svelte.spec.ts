import { createRawSnippet, tick } from 'svelte';
import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { AccessConnectionState, AccessSession } from '$lib/access';
import AppShell from './AppShell.svelte';

const connection = vi.hoisted(() => ({ emit: (_state: AccessConnectionState) => {} }));
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
