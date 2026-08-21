import type { CreateRequest, CreateResponse } from './types';

async function postEmpty(path: string, body?: unknown): Promise<void> {
	const res = await fetch(
		path,
		body === undefined
			? { method: 'POST' }
			: {
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify(body)
				}
	);
	if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
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
