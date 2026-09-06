import type { CardConfig } from './config';

type ConnectionConfig = Pick<CardConfig, 'endpoint' | 'token'>;
type DirectSessionResponse = { session_id: string; answer: { type: 'answer'; sdp: string } };
export const requestTimeoutMs = 10_000;

export class CardConnectionError extends Error {
	constructor(
		readonly kind: 'authentication' | 'network' | 'protocol',
		message: string
	) {
		super(message);
		this.name = 'CardConnectionError';
	}
}

export function connectionError(error: unknown): CardConnectionError {
	if (error instanceof CardConnectionError) return error;
	const origin = globalThis.location?.origin ?? 'your Home Assistant origin';
	return new CardConnectionError(
		'network',
		`Cannot connect. Check the endpoint, TLS certificate, browser Local Network Access permission, and direct_card.allowed_origins for ${origin}.`
	);
}

async function post(
	config: ConnectionConfig,
	path: string,
	options: RequestInit
): Promise<Response> {
	try {
		const response = await fetch(`${config.endpoint}/${path}`, {
			...options,
			method: 'POST',
			mode: 'cors',
			credentials: 'omit',
			redirect: 'error',
			cache: 'no-store',
			referrerPolicy: 'no-referrer',
			signal: AbortSignal.timeout(requestTimeoutMs),
			headers: {
				'Content-Type': 'application/json',
				Authorization: `Bearer ${config.token}`,
				...options.headers
			}
		});
		if (response.status === 401 || response.status === 403) {
			await response.body?.cancel();
			throw new CardConnectionError(
				'authentication',
				'Access denied. Check or replace the card access key.'
			);
		}
		if (!response.ok && !(path === 'delete' && response.status === 404)) {
			await response.body?.cancel();
			throw new CardConnectionError(
				'protocol',
				'KeepPeek rejected the session request. Check the endpoint and card/server versions.'
			);
		}
		return response;
	} catch (error) {
		throw connectionError(error);
	}
}

function invalidResponse(): CardConnectionError {
	return new CardConnectionError(
		'protocol',
		'KeepPeek returned an invalid or oversized session response.'
	);
}

async function readResponse(response: Response): Promise<unknown> {
	if (!response.body) throw invalidResponse();
	const reader = response.body.getReader();
	const decoder = new TextDecoder();
	let bytes = 0;
	let text = '';
	try {
		for (let count = 0; count < 4096; count += 1) {
			const chunk = await reader.read();
			if (chunk.done) return JSON.parse(text + decoder.decode());
			bytes += chunk.value.byteLength;
			if (bytes > 1_048_576) break;
			text += decoder.decode(chunk.value, { stream: true });
		}
		await reader.cancel();
		throw invalidResponse();
	} catch {
		throw invalidResponse();
	} finally {
		reader.releaseLock();
	}
}

export async function createDirectSession(
	config: ConnectionConfig,
	offer: RTCSessionDescriptionInit
): Promise<DirectSessionResponse> {
	if (typeof CompressionStream === 'undefined') {
		throw new CardConnectionError(
			'protocol',
			'This browser needs CompressionStream support. Update the browser.'
		);
	}
	if (offer.type !== 'offer' || !offer.sdp || offer.sdp.length > 262_144) throw invalidResponse();
	const compressed = new Blob([JSON.stringify({ offer })])
		.stream()
		.pipeThrough(new CompressionStream('gzip'));
	const response = await post(config, 'create', {
		headers: { 'Content-Encoding': 'gzip' },
		body: await new Response(compressed).arrayBuffer()
	});
	const value = (await readResponse(response)) as Partial<DirectSessionResponse> | null;
	if (
		!value ||
		typeof value.session_id !== 'string' ||
		!value.session_id ||
		value.session_id.length > 160
	) {
		throw invalidResponse();
	}
	if (
		value.answer?.type !== 'answer' ||
		typeof value.answer.sdp !== 'string' ||
		!value.answer.sdp
	) {
		await deleteDirectSession(config, value.session_id);
		throw invalidResponse();
	}
	return { session_id: value.session_id, answer: value.answer };
}

export async function deleteDirectSession(
	config: ConnectionConfig,
	sessionId: string
): Promise<void> {
	const response = await post(config, 'delete', {
		body: JSON.stringify({ session_id: sessionId }),
		keepalive: true
	});
	await response.body?.cancel();
}
