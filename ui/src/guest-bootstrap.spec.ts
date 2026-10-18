import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';
import { describe, expect, it, vi } from 'vitest';

const template = readFileSync(fileURLToPath(new URL('./app.html', import.meta.url)), 'utf8');
const token = 'A'.repeat(43);

type Handoff = { kind: 'none' | 'invalid' | 'token'; token?: string };

function boot(href: string) {
	let address = new URL(href);
	const localStorage = { getItem: vi.fn(() => 'light'), setItem: vi.fn() };
	const sessionStorage = { getItem: vi.fn(), setItem: vi.fn() };
	const dataset: Record<string, string> = {};
	const document = { documentElement: { dataset, classList: { add: vi.fn(), toggle: vi.fn() } } };
	const history = {
		state: null,
		replaceState: vi.fn((_state: unknown, _title: string, url: string) => {
			address = new URL(url, address);
		})
	};
	const listeners = new Map<string, Set<(event: Event) => void>>();
	const window = {
		get location() {
			return address;
		},
		__keeppeekTakeGuestToken: undefined as (() => Handoff) | undefined,
		addEventListener(name: string, listener: (event: Event) => void) {
			const group = listeners.get(name) ?? new Set();
			group.add(listener);
			listeners.set(name, group);
		},
		dispatchEvent(event: Event) {
			for (const listener of listeners.get(event.type) ?? []) listener(event);
			return true;
		}
	};
	const script = template.match(/<script>([\s\S]*?)<\/script>/)?.[1];
	if (!script) throw new Error('Trusted bootstrap is missing');
	runInNewContext(script, {
		window,
		history,
		document,
		localStorage,
		sessionStorage,
		matchMedia: () => ({ matches: false }),
		URL,
		Event
	});
	return {
		window,
		history,
		localStorage,
		sessionStorage,
		dataset,
		url: () => address,
		navigate(fragment: string) {
			address = new URL(fragment, address);
			window.dispatchEvent(new Event('hashchange'));
		}
	};
}

describe('Early guest handoff', () => {
	it('removes the raw fragment before framework startup and exposes it once without storage', () => {
		const state = boot(`https://recorder.example/share#${token}`);
		expect(state.url().hash).toBe('');
		expect(state.window.__keeppeekTakeGuestToken?.()).toEqual({ kind: 'token', token });
		expect(state.window.__keeppeekTakeGuestToken?.()).toEqual({ kind: 'invalid' });
		expect(state.localStorage.getItem).not.toHaveBeenCalled();
		expect(state.localStorage.setItem).not.toHaveBeenCalled();
		expect(state.sessionStorage.getItem).not.toHaveBeenCalled();
		expect(state.sessionStorage.setItem).not.toHaveBeenCalled();
		expect(template.indexOf('<script>')).toBeLessThan(template.indexOf('%sveltekit.head%'));
	});

	it.each(['#bad', '#', `#${'B'.repeat(44)}`, `#${'%41'.repeat(43)}`])(
		'keeps invalid fragment %s distinct from a cookie-resumable reload',
		(fragment) => {
			const state = boot(`https://recorder.example/share${fragment}`);
			expect(state.url().hash).toBe('');
			expect(state.window.__keeppeekTakeGuestToken?.()).toEqual({ kind: 'invalid' });
		}
	);

	it('permits cookie resume only when the guest document had no fragment', () => {
		const state = boot('https://recorder.example/share');
		expect(state.window.__keeppeekTakeGuestToken?.()).toEqual({ kind: 'none' });
		expect(state.dataset.theme).toBe('dark');
	});

	it('scrubs a later hash before downstream listeners and keeps its invalid context distinct', () => {
		const state = boot('https://recorder.example/share');
		state.window.__keeppeekTakeGuestToken?.();
		let observedHash = 'unobserved';
		state.window.addEventListener('hashchange', () => {
			observedHash = state.url().hash;
		});
		state.navigate('#bad');
		expect(observedHash).toBe('');
		expect(state.window.__keeppeekTakeGuestToken?.()).toEqual({ kind: 'invalid' });
		state.navigate(`#${token}`);
		expect(state.window.__keeppeekTakeGuestToken?.()).toEqual({ kind: 'token', token });
		expect(state.sessionStorage.setItem).not.toHaveBeenCalled();
	});

	it('preserves the normal stored theme bootstrap outside the exact guest route', () => {
		const state = boot(`https://recorder.example/keep#${token}`);
		expect(state.url().hash).toBe(`#${token}`);
		expect(state.window.__keeppeekTakeGuestToken).toBeUndefined();
		expect(state.localStorage.getItem).toHaveBeenCalledWith('keeppeek-theme');
		expect(state.dataset.theme).toBe('light');
	});
});
