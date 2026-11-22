import { describe, expect, it, vi } from 'vitest';
import { BrowserLogStore, installConsoleCapture } from './browser-logs';

class MemoryStorage implements Storage {
	private values = new Map<string, string>();

	get length(): number {
		return this.values.size;
	}

	clear(): void {
		this.values.clear();
	}

	getItem(key: string): string | null {
		return this.values.get(key) ?? null;
	}

	key(index: number): string | null {
		return [...this.values.keys()][index] ?? null;
	}

	removeItem(key: string): void {
		this.values.delete(key);
	}

	setItem(key: string, value: string): void {
		this.values.set(key, value);
	}
}

function fakeConsole(log = vi.fn()): Console {
	return {
		debug: vi.fn(),
		info: vi.fn(),
		log,
		warn: vi.fn(),
		error: vi.fn()
	} as unknown as Console;
}

describe('BrowserLogStore', () => {
	it('captures console output once while preserving the original call', () => {
		const store = new BrowserLogStore({ storage: null });
		const originalLog = vi.fn();
		const target = fakeConsole(originalLog);
		const cleanup = installConsoleCapture(store, target);
		installConsoleCapture(store, target);

		target.log('camera ready', { id: 'front' });

		expect(originalLog).toHaveBeenCalledWith('camera ready', { id: 'front' });
		expect(store.snapshot()).toHaveLength(1);
		expect(store.snapshot()[0]).toMatchObject({
			level: 'info',
			target: 'browser.console.log',
			message: 'camera ready {"id":"front"}'
		});
		cleanup();
	});

	it('formats circular values and redacts credentials', () => {
		const store = new BrowserLogStore({ storage: null });
		const circular: Record<string, unknown> = {
			password: 'camera-secret',
			url: 'rtsp://operator:password@192.0.2.1/live'
		};
		circular.self = circular;

		store.append('error', 'browser.test', [circular, 'token=abc123']);

		const message = store.snapshot()[0].message;
		expect(message).toContain('"password":"[REDACTED]"');
		expect(message).toContain('rtsp://[REDACTED]@192.0.2.1/live');
		expect(message).toContain('[Circular]');
		expect(message).toContain('token=[REDACTED]');
		expect(message).not.toContain('camera-secret');
		expect(message).not.toContain('abc123');
	});

	it('restores bounded history from the current tab session', () => {
		const storage = new MemoryStorage();
		const first = new BrowserLogStore({ storage, maxEntries: 2 });
		first.append('info', 'browser.test', ['one']);
		first.append('warn', 'browser.test', ['two']);
		first.append('error', 'browser.test', ['three']);

		const restored = new BrowserLogStore({ storage, maxEntries: 2 });

		expect(restored.snapshot().map((entry) => entry.message)).toEqual(['two', 'three']);
		expect(restored.append('info', 'browser.test', ['four']).sequence).toBe(4);
	});

	it('keeps in-memory history when session storage rejects writes', () => {
		const storage = new MemoryStorage();
		storage.setItem = () => {
			throw new DOMException('quota exceeded', 'QuotaExceededError');
		};
		const store = new BrowserLogStore({ storage });

		store.append('warn', 'browser.test', ['still available']);

		expect(store.snapshot()[0].message).toBe('still available');
	});
});

describe('Owned browser logging lifecycle', () => {
	it('releases hooks only after the last owner and restores exact console functions', async () => {
		const { initializeBrowserLogging, browserLogStore } = await import('./browser-logs');
		const target = fakeConsole();
		const original = target.log;
		const listeners = new Map<string, Set<EventListener>>();
		const fakeWindow = {
			console: target,
			addEventListener: vi.fn((name: string, listener: EventListener) => {
				const group = listeners.get(name) ?? new Set();
				group.add(listener);
				listeners.set(name, group);
			}),
			removeEventListener: vi.fn((name: string, listener: EventListener) => {
				listeners.get(name)?.delete(listener);
			})
		};
		vi.stubGlobal('window', fakeWindow);
		const first = initializeBrowserLogging();
		const second = initializeBrowserLogging();
		first();
		const before = browserLogStore.snapshot().length;
		target.log('still owned');
		const during = browserLogStore.snapshot().length;
		second();
		second();
		target.log('after disposal');
		vi.unstubAllGlobals();
		expect(during).toBe(before + 1);
		expect(browserLogStore.snapshot()).toHaveLength(during);
		expect(target.log).toBe(original);
		expect([...listeners.values()].every((group) => group.size === 0)).toBe(true);
		expect(fakeWindow.addEventListener).toHaveBeenCalledTimes(2);
		expect(fakeWindow.removeEventListener).toHaveBeenCalledTimes(2);
	});

	it('redacts issued share URLs in message, object, debug, header and source-file paths', () => {
		const store = new BrowserLogStore({ storage: null });
		const token = 'A'.repeat(43);
		const url = `https://recorder.example/share#${token}`;
		store.append(
			'error',
			'browser.test',
			[
				url,
				{ url, token },
				`IssuedShareGrant { token: "${token}" }`,
				`Authorization: Bearer ${token}`
			],
			{ file: url, stack: `at ${url}`, fields: { response: { share_url: url } } }
		);
		expect(JSON.stringify(store.snapshot())).not.toContain(token);
		expect(store.snapshot()[0].message).toContain('[REDACTED]');
	});
});
