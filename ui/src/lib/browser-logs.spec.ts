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
