import type { CreateRequest, CreateResponse, LogSnapshot, ServerLogEntry } from './types';

export async function fetchLogSnapshot(): Promise<LogSnapshot> {
	const response = await fetch('/logs/snapshot', {
		headers: { Accept: 'application/json' },
		cache: 'no-store'
	});
	if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
	const value: unknown = await response.json();
	if (!isLogSnapshot(value)) throw new Error('Server returned an invalid log snapshot.');
	return value;
}

export async function fetchMetricsSnapshot(): Promise<string> {
	const response = await fetch('/metrics', {
		headers: { Accept: 'text/plain' },
		cache: 'no-store'
	});
	if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
	return response.text();
}

async function postEmpty(path: string, body?: unknown): Promise<void> {
	const res = await fetch(
		path,
		body === undefined
			? { method: 'POST' }
			: {
					method: 'POST',
					headers: {
						'Content-Type': 'application/json',
						Prefer: 'return=representation'
					},
					body: JSON.stringify(body)
				}
	);
	if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
	await res.text();
}

export async function waitForMetricsAt(origin: string): Promise<void> {
	const url = new URL('/metrics', origin);
	const crossOrigin = url.origin !== window.location.origin;
	const response = await fetch(url, crossOrigin ? { mode: 'no-cors' } : undefined);
	if (!crossOrigin && !response.ok) throw new Error(`${response.status} ${response.statusText}`);
}

export async function createSession(offer: RTCSessionDescriptionInit): Promise<CreateResponse> {
	const request: CreateRequest = { offer: { type: offer.type as string, sdp: offer.sdp! } };
	const requestString = JSON.stringify(request);

	let body: ArrayBuffer | Uint8Array;
	if (typeof CompressionStream !== 'undefined') {
		const stream = new Blob([requestString])
			.stream()
			.pipeThrough(
				new /* eslint-disable-next-line @typescript-eslint/no-explicit-any */ (
					window as any
				).CompressionStream('gzip')
			);
		body = await new Response(stream).arrayBuffer();
	} else {
		throw new Error('CompressionStream not supported in this environment');
	}

	const res = await fetch('/create', {
		method: 'POST',
		headers: {
			'Content-Type': 'application/json',
			'Content-Encoding': 'gzip'
		},
		body
	});

	if (!res.ok) {
		const message = await res.text();
		throw new Error(`${res.status} ${message || res.statusText}`);
	}

	return res.json();
}

export function deleteSession(sessionId: string): Promise<void> {
	return postEmpty('/delete', { session_id: sessionId });
}

function isLogSnapshot(value: unknown): value is LogSnapshot {
	if (!value || typeof value !== 'object') return false;
	const snapshot = value as Partial<LogSnapshot>;
	return (
		Array.isArray(snapshot.entries) &&
		snapshot.entries.every(isServerLogEntry) &&
		(snapshot.oldest_sequence === null || typeof snapshot.oldest_sequence === 'number') &&
		(snapshot.newest_sequence === null || typeof snapshot.newest_sequence === 'number') &&
		typeof snapshot.truncated === 'boolean' &&
		Boolean(snapshot.stats) &&
		typeof snapshot.stats?.entry_count === 'number' &&
		typeof snapshot.stats?.byte_count === 'number' &&
		typeof snapshot.stats?.evicted_entries === 'number' &&
		typeof snapshot.stats?.max_entries === 'number' &&
		typeof snapshot.stats?.max_bytes === 'number'
	);
}

function isServerLogEntry(value: unknown): value is ServerLogEntry {
	if (!value || typeof value !== 'object') return false;
	const entry = value as Partial<ServerLogEntry>;
	return (
		typeof entry.sequence === 'number' &&
		typeof entry.timestamp_ms === 'number' &&
		typeof entry.level === 'string' &&
		typeof entry.target === 'string' &&
		typeof entry.message === 'string' &&
		Boolean(entry.fields) &&
		typeof entry.fields === 'object'
	);
}
