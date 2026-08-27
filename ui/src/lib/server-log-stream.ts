import type { ServerLogEntry } from './types';

export type LogStreamState = 'connecting' | 'connected' | 'reconnecting' | 'closed';

export interface LogStreamCallbacks {
	onentry: (entry: ServerLogEntry) => void;
	onstate: (state: LogStreamState) => void;
	ongap: (dropped: number) => void;
	onreplaytruncated: () => void;
}

export type LogStreamOpener = (url: string, signal: AbortSignal) => Promise<Response>;

type SseEvent = {
	event: string;
	data: string;
	id: string | null;
};

const reconnectDelayMs = 1_000;

export class ServerLogStream {
	private controller: AbortController | null = null;
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	private generation = 0;
	private running = false;
	private after: number | undefined;
	private tail = 200;

	constructor(
		private readonly callbacks: LogStreamCallbacks,
		private readonly openStream: LogStreamOpener = openLocalLogStream
	) {}

	start(after?: number, tail = 200): void {
		this.close(false);
		this.running = true;
		this.after = after;
		this.tail = tail;
		this.callbacks.onstate('connecting');
		void this.connect(this.generation);
	}

	close(reportState = true): void {
		this.running = false;
		this.generation += 1;
		this.controller?.abort();
		this.controller = null;
		if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
		this.reconnectTimer = null;
		if (reportState) this.callbacks.onstate('closed');
	}

	private async connect(generation: number): Promise<void> {
		const params = new URLSearchParams({ tail: String(this.tail) });
		if (this.after !== undefined) params.set('after', String(this.after));
		const controller = new AbortController();
		this.controller = controller;
		try {
			const response = await this.openStream(`/logs?${params.toString()}`, controller.signal);
			if (!this.current(generation)) return;
			if (!response.ok) throw new Error(`Server log stream failed with HTTP ${response.status}.`);
			if (!response.body) throw new Error('Server log stream has no response body.');
			this.callbacks.onstate('connected');
			await consumeSse(response.body, controller.signal, (event) => this.dispatch(event));
			if (this.current(generation)) this.reconnect(generation);
		} catch (error) {
			if (this.current(generation) && !isAbortError(error)) this.reconnect(generation);
		}
	}

	private dispatch(event: SseEvent): void {
		if (event.id !== null) {
			const sequence = Number(event.id);
			if (Number.isSafeInteger(sequence) && sequence >= 0) this.after = sequence;
		}
		if (event.event === 'log') {
			const entry = parseServerLogEntry(event.data);
			if (entry) this.callbacks.onentry(entry);
			return;
		}
		if (event.event === 'gap') {
			try {
				const data: unknown = JSON.parse(event.data);
				if (
					data &&
					typeof data === 'object' &&
					typeof (data as { dropped?: unknown }).dropped === 'number'
				) {
					this.callbacks.ongap((data as { dropped: number }).dropped);
				}
			} catch {
				return;
			}
			return;
		}
		if (event.event === 'replay-truncated') this.callbacks.onreplaytruncated();
	}

	private reconnect(generation: number): void {
		this.callbacks.onstate('reconnecting');
		this.reconnectTimer = setTimeout(() => {
			this.reconnectTimer = null;
			if (this.current(generation)) void this.connect(generation);
		}, reconnectDelayMs);
	}

	private current(generation: number): boolean {
		return this.running && this.generation === generation;
	}
}

async function openLocalLogStream(url: string, signal: AbortSignal): Promise<Response> {
	return fetch(url, {
		headers: { Accept: 'text/event-stream' },
		cache: 'no-store',
		signal
	});
}

async function consumeSse(
	body: ReadableStream<Uint8Array>,
	signal: AbortSignal,
	onEvent: (event: SseEvent) => void
): Promise<void> {
	const reader = body.getReader();
	const decoder = new TextDecoder();
	let pending = '';
	try {
		while (!signal.aborted) {
			const { done, value } = await reader.read();
			if (done) break;
			pending += decoder.decode(value, { stream: true });
			let boundary = eventBoundary(pending);
			while (boundary) {
				const frame = pending.slice(0, boundary.index);
				pending = pending.slice(boundary.index + boundary.length);
				const event = parseSseEvent(frame);
				if (event) onEvent(event);
				boundary = eventBoundary(pending);
			}
		}
	} finally {
		reader.releaseLock();
	}
}

function eventBoundary(value: string): { index: number; length: number } | null {
	const match = /\r?\n\r?\n/.exec(value);
	return match ? { index: match.index, length: match[0].length } : null;
}

function parseSseEvent(frame: string): SseEvent | null {
	let event = 'message';
	let id: string | null = null;
	const data: string[] = [];
	for (const line of frame.split(/\r?\n/)) {
		if (line.startsWith(':')) continue;
		const separator = line.indexOf(':');
		const field = separator === -1 ? line : line.slice(0, separator);
		const value = separator === -1 ? '' : line.slice(separator + 1).replace(/^ /, '');
		if (field === 'event') event = value;
		else if (field === 'data') data.push(value);
		else if (field === 'id' && !value.includes('\0')) id = value;
	}
	if (data.length === 0) return null;
	return { event, data: data.join('\n'), id };
}

function parseServerLogEntry(data: string): ServerLogEntry | null {
	try {
		const entry: unknown = JSON.parse(data);
		if (!entry || typeof entry !== 'object') return null;
		const candidate = entry as Partial<ServerLogEntry>;
		if (
			typeof candidate.sequence !== 'number' ||
			typeof candidate.timestamp_ms !== 'number' ||
			typeof candidate.level !== 'string' ||
			typeof candidate.target !== 'string' ||
			typeof candidate.message !== 'string' ||
			!candidate.fields ||
			typeof candidate.fields !== 'object'
		) {
			return null;
		}
		return candidate as ServerLogEntry;
	} catch {
		return null;
	}
}

function isAbortError(error: unknown): boolean {
	return error instanceof DOMException && error.name === 'AbortError';
}
