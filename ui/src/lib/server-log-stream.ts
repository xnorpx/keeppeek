import type { ServerLogEntry } from './types';

export type LogStreamState = 'connecting' | 'connected' | 'reconnecting' | 'closed';

export interface LogStreamCallbacks {
	onentry: (entry: ServerLogEntry) => void;
	onstate: (state: LogStreamState) => void;
	ongap: (dropped: number) => void;
	onreplaytruncated: () => void;
}

type EventSourceFactory = (url: string) => EventSource;

export class ServerLogStream {
	private source: EventSource | null = null;

	constructor(
		private readonly callbacks: LogStreamCallbacks,
		private readonly createEventSource: EventSourceFactory = (url) => new EventSource(url)
	) {}

	start(after?: number, tail = 200): void {
		this.close(false);
		const params = new URLSearchParams({ tail: String(tail) });
		if (after !== undefined) params.set('after', String(after));
		this.callbacks.onstate('connecting');
		const source = this.createEventSource(`/logs?${params.toString()}`);
		this.source = source;
		source.onopen = () => this.callbacks.onstate('connected');
		source.onerror = () => this.callbacks.onstate('reconnecting');
		source.addEventListener('log', (event) => {
			const entry = parseServerLogEntry((event as MessageEvent<string>).data);
			if (entry) this.callbacks.onentry(entry);
		});
		source.addEventListener('gap', (event) => {
			try {
				const data: unknown = JSON.parse((event as MessageEvent<string>).data);
				if (
					data &&
					typeof data === 'object' &&
					typeof (data as { dropped?: unknown }).dropped === 'number'
				) {
					this.callbacks.ongap((data as { dropped: number }).dropped);
				}
			} catch {
				// Ignore malformed control events and keep the stream alive.
			}
		});
		source.addEventListener('replay-truncated', () => this.callbacks.onreplaytruncated());
	}

	close(reportState = true): void {
		this.source?.close();
		this.source = null;
		if (reportState) this.callbacks.onstate('closed');
	}
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
