import { maxSources, parseEndpoint, type CardConfig, type CardSource } from './config';

export type SourceChoice = { source_id: string; title: string; available: boolean };
export type StreamState = {
	status: 'connecting' | 'live' | 'unavailable';
	stream: MediaStream | null;
	message: string | null;
};
export type SessionSnapshot = {
	status: 'connecting' | 'ready' | 'reconnecting' | 'error' | 'closed';
	message: string | null;
	cameras: readonly SourceChoice[];
	streams: ReadonlyMap<string, StreamState>;
};
export type SessionAdapter = {
	configure: (sources: readonly CardSource[]) => void;
	retry: () => void;
	close: () => Promise<void>;
};
export type SessionFactory = (
	config: Pick<CardConfig, 'endpoint' | 'token'>,
	onchange: (snapshot: SessionSnapshot) => void
) => SessionAdapter;
export type CardLease = {
	release: () => Promise<void>;
	retry: () => void;
	updateSources: (sources: readonly CardSource[]) => void;
};
type Consumer = { sources: readonly CardSource[]; notify: (snapshot: SessionSnapshot) => void };
type Entry = {
	session: SessionAdapter;
	consumers: Set<Consumer>;
	snapshot: SessionSnapshot;
};

export function sourceKey(source: CardSource): string {
	return JSON.stringify([source.source_id, source.quality]);
}

function desiredSources(consumers: Iterable<Consumer>): CardSource[] {
	const sources = new Map<string, CardSource>();
	for (const consumer of consumers) {
		for (const source of consumer.sources) sources.set(sourceKey(source), source);
	}
	if (sources.size > maxSources) {
		throw new Error(`A shared KeepPeek connection supports at most ${maxSources} live sources.`);
	}
	return [...sources.values()];
}

export class KeepPeekConnectionManager {
	#entries = new Map<string, Entry>();
	#closing = new Map<string, Promise<void>>();
	#pending = 0;
	#factory: SessionFactory;

	constructor(factory: SessionFactory) {
		this.#factory = factory;
	}

	async acquire(
		config: Pick<CardConfig, 'endpoint' | 'token' | 'sources'>,
		notify: Consumer['notify'],
		signal?: AbortSignal
	): Promise<CardLease> {
		if (this.#pending >= 64) throw new Error('Too many cards are connecting.');
		this.#pending += 1;
		try {
			const endpoint = parseEndpoint(config.endpoint);
			const fingerprint = await crypto.subtle.digest(
				'SHA-256',
				new TextEncoder().encode(config.token)
			);
			const identity = JSON.stringify([endpoint, Array.from(new Uint8Array(fingerprint))]);
			await this.#closing.get(identity);
			if (signal?.aborted) throw new Error('Card was removed before connecting.');
			return this.attach(identity, { ...config, endpoint }, notify);
		} finally {
			this.#pending -= 1;
		}
	}

	private attach(
		identity: string,
		config: Pick<CardConfig, 'endpoint' | 'token' | 'sources'>,
		notify: Consumer['notify']
	): CardLease {
		let entry = this.#entries.get(identity);
		const consumer: Consumer = { sources: config.sources, notify };
		if (entry && entry.consumers.size >= 64)
			throw new Error('A connection supports at most 64 cards.');
		const sources = desiredSources([...(entry?.consumers ?? []), consumer]);
		if (!entry) {
			if (this.#entries.size + this.#closing.size >= 8) {
				throw new Error('A dashboard supports at most 8 KeepPeek connection identities.');
			}
			entry = this.createEntry(config);
			this.#entries.set(identity, entry);
		}
		const ownedEntry = entry;
		entry.consumers.add(consumer);
		notify(entry.snapshot);
		entry.session.configure(sources);
		let released = false;
		return {
			updateSources: (nextSources) => {
				if (released) return;
				const next = desiredSources(
					[...ownedEntry.consumers].map((current) =>
						current === consumer ? { sources: nextSources, notify } : current
					)
				);
				consumer.sources = nextSources;
				ownedEntry.session.configure(next);
			},
			retry: () => {
				if (!released) ownedEntry.session.retry();
			},
			release: async () => {
				if (released) return;
				released = true;
				await this.release(identity, ownedEntry, consumer);
			}
		};
	}

	private async release(identity: string, entry: Entry, consumer: Consumer): Promise<void> {
		entry.consumers.delete(consumer);
		if (entry.consumers.size > 0) {
			entry.session.configure(desiredSources(entry.consumers));
			return;
		}
		this.#entries.delete(identity);
		const closing = Promise.resolve().then(() => entry.session.close());
		this.#closing.set(identity, closing);
		try {
			await closing;
		} finally {
			this.#closing.delete(identity);
		}
	}

	private createEntry(config: Pick<CardConfig, 'endpoint' | 'token'>): Entry {
		const consumers = new Set<Consumer>();
		const entry = {
			consumers,
			snapshot: {
				status: 'connecting',
				message: null,
				cameras: [],
				streams: new Map()
			} as SessionSnapshot,
			session: this.#factory(config, (snapshot) => {
				entry.snapshot = snapshot;
				for (const consumer of consumers) consumer.notify(snapshot);
			})
		};
		return entry;
	}
}
