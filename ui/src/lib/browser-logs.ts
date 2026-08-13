import type { BrowserLogEntry, LogLevel } from './types';

const STORAGE_KEY = 'keeppeek.browser-logs.v1';
const DEFAULT_MAX_ENTRIES = 2_000;
const DEFAULT_MAX_BYTES = 2 * 1024 * 1024;
const MAX_VALUE_CHARS = 16_000;
const consoleMethods = ['debug', 'info', 'log', 'warn', 'error'] as const;

type ConsoleMethod = (typeof consoleMethods)[number];
type BrowserLogListener = (entry: BrowserLogEntry | null) => void;

export interface BrowserLogStoreOptions {
	storage?: Storage | null;
	maxEntries?: number;
	maxBytes?: number;
}

export class BrowserLogStore {
	private entries: BrowserLogEntry[] = [];
	private listeners = new Set<BrowserLogListener>();
	private nextSequence = 1;
	private readonly storage: Storage | null;
	private readonly maxEntries: number;
	private readonly maxBytes: number;

	constructor(options: BrowserLogStoreOptions = {}) {
		this.storage = options.storage === undefined ? browserSessionStorage() : options.storage;
		this.maxEntries = Math.max(1, options.maxEntries ?? DEFAULT_MAX_ENTRIES);
		this.maxBytes = Math.max(1, options.maxBytes ?? DEFAULT_MAX_BYTES);
		this.restore();
	}

	append(
		level: LogLevel,
		target: string,
		values: unknown[],
		details: Partial<Pick<BrowserLogEntry, 'source' | 'stack' | 'file' | 'line' | 'fields'>> = {}
	): BrowserLogEntry {
		const entry: BrowserLogEntry = {
			sequence: this.nextSequence,
			timestamp_ms: Date.now(),
			level,
			target,
			message: redactText(formatValues(values)).slice(0, MAX_VALUE_CHARS),
			fields: redactValue(details.fields ?? {}) as Record<string, unknown>,
			source: details.source ?? 'console',
			...(details.stack ? { stack: redactText(details.stack).slice(0, MAX_VALUE_CHARS) } : {}),
			...(details.file ? { file: details.file } : {}),
			...(details.line !== undefined ? { line: details.line } : {})
		};
		this.nextSequence += 1;
		this.entries.push(entry);
		this.enforceBounds();
		this.persist();
		for (const listener of this.listeners) listener(entry);
		return entry;
	}

	snapshot(): BrowserLogEntry[] {
		return this.entries.map((entry) => structuredClone(entry));
	}

	clear(): void {
		this.entries = [];
		this.persist();
		for (const listener of this.listeners) listener(null);
	}

	subscribe(listener: BrowserLogListener): () => void {
		this.listeners.add(listener);
		return () => this.listeners.delete(listener);
	}

	private enforceBounds(): void {
		while (this.entries.length > this.maxEntries || serializedBytes(this.entries) > this.maxBytes) {
			this.entries.shift();
		}
	}

	private restore(): void {
		if (!this.storage) return;
		try {
			const stored = this.storage.getItem(STORAGE_KEY);
			if (!stored) return;
			const entries: unknown = JSON.parse(stored);
			if (!Array.isArray(entries)) return;
			this.entries = entries.filter(isBrowserLogEntry).slice(-this.maxEntries);
			this.enforceBounds();
			this.nextSequence = (this.entries.at(-1)?.sequence ?? 0) + 1;
		} catch {
			this.entries = [];
		}
	}

	private persist(): void {
		if (!this.storage) return;
		try {
			this.storage.setItem(STORAGE_KEY, JSON.stringify(this.entries));
		} catch {
			// Keep the in-memory history when storage is unavailable or full.
		}
	}
}

const installedConsoles = new WeakMap<object, () => void>();

export function installConsoleCapture(store: BrowserLogStore, target: Console): () => void {
	const installed = installedConsoles.get(target);
	if (installed) return () => {};
	let recording = false;
	const originals = new Map<ConsoleMethod, (...values: unknown[]) => void>();
	for (const method of consoleMethods) {
		const original = target[method].bind(target) as (...values: unknown[]) => void;
		originals.set(method, original);
		target[method] = ((...values: unknown[]) => {
			original(...values);
			if (recording) return;
			recording = true;
			try {
				store.append(consoleLevel(method), `browser.console.${method}`, values);
			} finally {
				recording = false;
			}
		}) as Console[typeof method];
	}

	const cleanup = () => {
		for (const method of consoleMethods) {
			const original = originals.get(method);
			if (original) target[method] = original as Console[typeof method];
		}
		installedConsoles.delete(target);
	};
	installedConsoles.set(target, cleanup);
	return cleanup;
}

export const browserLogStore = new BrowserLogStore();

let initialized = false;

export function initializeBrowserLogging(): void {
	if (initialized || typeof window === 'undefined') return;
	initialized = true;
	installConsoleCapture(browserLogStore, window.console);
	window.addEventListener('error', (event) => {
		browserLogStore.append('error', 'browser.window.error', [event.message], {
			source: 'window-error',
			stack: event.error instanceof Error ? event.error.stack : undefined,
			file: event.filename || undefined,
			line: event.lineno || undefined,
			fields: { column: event.colno || undefined }
		});
	});
	window.addEventListener('unhandledrejection', (event) => {
		const reason = event.reason;
		browserLogStore.append('error', 'browser.promise.unhandled', [reason], {
			source: 'unhandled-rejection',
			stack: reason instanceof Error ? reason.stack : undefined
		});
	});
}

function browserSessionStorage(): Storage | null {
	if (typeof window === 'undefined') return null;
	try {
		return window.sessionStorage;
	} catch {
		return null;
	}
}

function consoleLevel(method: ConsoleMethod): LogLevel {
	switch (method) {
		case 'debug':
			return 'debug';
		case 'warn':
			return 'warn';
		case 'error':
			return 'error';
		default:
			return 'info';
	}
}

function formatValues(values: unknown[]): string {
	return values.map((value) => formatValue(value)).join(' ');
}

function formatValue(value: unknown): string {
	if (typeof value === 'string') return value;
	if (value instanceof Error) return value.stack ?? value.message;
	if (typeof value === 'bigint') return `${value}n`;
	if (typeof value === 'symbol') return value.toString();
	if (typeof value === 'function') return `[Function ${value.name || 'anonymous'}]`;
	if (value === undefined) return 'undefined';
	try {
		const seen = new WeakSet<object>();
		return JSON.stringify(value, (key, nested) => {
			if (isSensitiveKey(key)) return '[REDACTED]';
			if (typeof nested === 'bigint') return `${nested}n`;
			if (nested && typeof nested === 'object') {
				if (seen.has(nested)) return '[Circular]';
				seen.add(nested);
			}
			return nested;
		});
	} catch {
		return String(value);
	}
}

function redactValue(value: unknown, key = '', seen = new WeakSet<object>()): unknown {
	if (isSensitiveKey(key)) return '[REDACTED]';
	if (typeof value === 'string') return redactText(value).slice(0, MAX_VALUE_CHARS);
	if (!value || typeof value !== 'object') return value;
	if (seen.has(value)) return '[Circular]';
	seen.add(value);
	if (Array.isArray(value)) return value.map((item) => redactValue(item, '', seen));
	return Object.fromEntries(
		Object.entries(value).map(([name, nested]) => [name, redactValue(nested, name, seen)])
	);
}

function redactText(value: string): string {
	return value
		.replace(/([a-z][a-z0-9+.-]*:\/\/)[^@\s/]+@/gi, '$1[REDACTED]@')
		.replace(
			/\b(password|passwd|secret|token|authorization|api[_-]?key)\s*=\s*([^\s,;&]+)/gi,
			'$1=[REDACTED]'
		);
}

function isSensitiveKey(key: string): boolean {
	return /password|passwd|secret|token|authorization|credential|api[_-]?key|cookie/i.test(key);
}

function serializedBytes(value: unknown): number {
	return new TextEncoder().encode(JSON.stringify(value)).byteLength;
}

function isBrowserLogEntry(value: unknown): value is BrowserLogEntry {
	if (!value || typeof value !== 'object') return false;
	const entry = value as Partial<BrowserLogEntry>;
	return (
		typeof entry.sequence === 'number' &&
		typeof entry.timestamp_ms === 'number' &&
		typeof entry.level === 'string' &&
		typeof entry.target === 'string' &&
		typeof entry.message === 'string' &&
		typeof entry.fields === 'object' &&
		typeof entry.source === 'string'
	);
}
