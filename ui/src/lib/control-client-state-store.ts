import { create } from '@bufbuild/protobuf';
import { durationFromMs } from '@bufbuild/protobuf/wkt';
import {
	DeleteStateSchema,
	GetStateSchema,
	PutStateSchema,
	StateStoreCommandSchema,
	StateStoreErrorCode,
	StateStoreUpdateKind,
	UnwatchStateSchema,
	WatchStateAckSchema,
	WatchStateSchema,
	type Ok,
	type Request,
	type StateDeleteResult,
	type StateEntry,
	type StateStoreCommand,
	type StateStoreWatchClosed,
	type StateStoreWatchUpdate,
	type StateWatchSnapshot
} from './proto/webrtc_pb';
import { StateStoreRequestError } from './state-store-error';

const maxWatchesPerClient = 8;
const maxRewatchAttempts = 3;
const defaultMaxEntriesPerWatch = 1024;

export type StateStoreWatchStatus = 'active' | 'rewatching' | 'disconnected' | 'closed' | 'failed';

export type StateStoreWatchListener = (entries: readonly StateEntry[]) => void;

export type StateStoreWatchOptions = {
	keyPrefix?: string;
	maxEntries?: number;
	listener?: StateStoreWatchListener;
};

export type StateStorePutOptions = {
	schema: string;
	value: StateEntry['value'];
	expectedRevision?: bigint;
	ttlMs?: number;
};

type SendStateStoreRequest = (command: Request['command']) => Promise<Ok['result']>;

type WatchRecord = {
	watchId: string;
	namespace: string;
	keyPrefix: string;
	maxEntries: number;
	listener: StateStoreWatchListener | undefined;
	entries: Map<string, StateEntry>;
	snapshotRevision: bigint;
	appliedSequence: bigint;
	status: StateStoreWatchStatus;
	generation: number;
	lastError: unknown;
	handle: StateStoreWatch | undefined;
};

export class StateStoreWatch {
	readonly #record: WatchRecord;
	readonly #close: () => Promise<void>;

	constructor(record: WatchRecord, close: () => Promise<void>) {
		this.#record = record;
		this.#close = close;
	}

	get watchId(): string {
		return this.#record.watchId;
	}

	get namespace(): string {
		return this.#record.namespace;
	}

	get keyPrefix(): string {
		return this.#record.keyPrefix;
	}

	get status(): StateStoreWatchStatus {
		return this.#record.status;
	}

	get appliedSequence(): bigint {
		return this.#record.appliedSequence;
	}

	get snapshotRevision(): bigint {
		return this.#record.snapshotRevision;
	}

	get lastError(): unknown {
		return this.#record.lastError;
	}

	entries(): readonly StateEntry[] {
		return [...this.#record.entries.values()];
	}

	get(key: string): StateEntry | undefined {
		return this.#record.entries.get(key);
	}

	close(): Promise<void> {
		return this.#close();
	}
}

export class ControlClientStateStore {
	readonly #sendRequest: SendStateStoreRequest;
	readonly #watches = new Map<string, StateStoreWatch>();
	readonly #maxWatches: number;
	readonly #defaultMaxEntries: number;
	#nextWatchNumber = 1;
	#disposed = false;

	constructor(
		sendRequest: SendStateStoreRequest,
		options: { maxWatches?: number; maxEntriesPerWatch?: number } = {}
	) {
		this.#sendRequest = sendRequest;
		this.#maxWatches = options.maxWatches ?? maxWatchesPerClient;
		this.#defaultMaxEntries = options.maxEntriesPerWatch ?? defaultMaxEntriesPerWatch;
	}

	get disposed(): boolean {
		return this.#disposed;
	}

	get watchCount(): number {
		return this.#watches.size;
	}

	async get(namespace: string, key: string): Promise<StateEntry> {
		this.throwIfDisposed();
		throwIfKeyInvalid(namespace, key);
		const result = await this.#sendRequest(
			stateStoreCommand({ case: 'get', value: create(GetStateSchema, { namespace, key }) })
		);
		if (result.case !== 'stateStoreResult' || result.value.result.case !== 'entry') {
			throw new Error('Server returned an unexpected state store response.');
		}
		const entry = result.value.result.value;
		if (entry.namespace !== namespace || entry.key !== key) {
			throw new Error('Server returned state for an unexpected key.');
		}
		return entry;
	}

	async put(namespace: string, key: string, put: StateStorePutOptions): Promise<StateEntry> {
		this.throwIfDisposed();
		throwIfKeyInvalid(namespace, key);
		if (put.schema.length === 0) throw new Error('State store put requires a schema.');
		const result = await this.#sendRequest(
			stateStoreCommand({
				case: 'put',
				value: create(PutStateSchema, {
					namespace,
					key,
					schema: put.schema,
					value: put.value,
					expectedRevision: put.expectedRevision,
					ttl: put.ttlMs === undefined ? undefined : durationFromMs(put.ttlMs)
				})
			})
		);
		if (result.case !== 'stateStoreResult' || result.value.result.case !== 'entry') {
			throw new Error('Server returned an unexpected state store response.');
		}
		const entry = result.value.result.value;
		if (entry.namespace !== namespace || entry.key !== key) {
			throw new Error('Server returned state for an unexpected key.');
		}
		return entry;
	}

	async delete(
		namespace: string,
		key: string,
		expectedRevision?: bigint
	): Promise<StateDeleteResult> {
		this.throwIfDisposed();
		throwIfKeyInvalid(namespace, key);
		const result = await this.#sendRequest(
			stateStoreCommand({
				case: 'delete',
				value: create(DeleteStateSchema, { namespace, key, expectedRevision })
			})
		);
		if (result.case !== 'stateStoreResult' || result.value.result.case !== 'deleted') {
			throw new Error('Server returned an unexpected state store response.');
		}
		return result.value.result.value;
	}

	async watch(namespace: string, options: StateStoreWatchOptions = {}): Promise<StateStoreWatch> {
		this.throwIfDisposed();
		if (namespace.length === 0) throw new Error('State store watch requires a namespace.');
		if (this.#watches.size >= this.#maxWatches) {
			throw new Error('State store watch limit reached.');
		}
		const keyPrefix = options.keyPrefix ?? '';
		const maxEntries = options.maxEntries ?? this.#defaultMaxEntries;
		if (maxEntries <= 0) throw new Error('State store watch requires a positive entry limit.');
		const watchId = this.allocateWatchId();
		const snapshot = await this.requestSnapshot(namespace, keyPrefix, watchId, maxEntries);
		if (this.#disposed) throw new Error('State store client is disposed.');
		const record: WatchRecord = {
			watchId,
			namespace,
			keyPrefix,
			maxEntries,
			listener: options.listener,
			entries: new Map(),
			snapshotRevision: 0n,
			appliedSequence: 0n,
			status: 'active',
			generation: 0,
			lastError: null,
			handle: undefined
		};
		const handle = trackedHandle(record, () => this.closeRecord(record));
		installSnapshot(record, snapshot);
		this.#watches.set(watchId, handle);
		return handle;
	}

	async handleWatchUpdate(update: StateStoreWatchUpdate): Promise<void> {
		if (this.#disposed) return;
		const handle = this.#watches.get(update.watchId);
		if (!handle) return;
		const record = recordOf(handle);
		if (record.status !== 'active') return;
		if (update.namespace !== record.namespace || !update.key.startsWith(record.keyPrefix)) return;
		if (!isApplicableUpdate(record, update)) {
			await this.rewatch(record, { unwatchFirst: true });
			return;
		}
		if (update.watchSequence <= record.appliedSequence) return;
		if (update.watchSequence !== record.appliedSequence + 1n) {
			await this.rewatch(record, { unwatchFirst: true });
			return;
		}
		applyUpdate(record, update);
		await this.acknowledge(record, update.watchSequence);
	}

	async handleWatchClosed(closed: StateStoreWatchClosed): Promise<void> {
		if (this.#disposed) return;
		const handle = this.#watches.get(closed.watchId);
		if (!handle) return;
		const record = recordOf(handle);
		if (record.status === 'closed') return;
		await this.rewatch(record, { unwatchFirst: false });
	}

	handleConnectionClosed(): void {
		for (const handle of this.#watches.values()) {
			const record = recordOf(handle);
			if (record.status === 'closed') continue;
			record.generation += 1;
			record.status = 'disconnected';
		}
	}

	async handleConnectionOpened(): Promise<void> {
		if (this.#disposed) return;
		const records = [...this.#watches.values()]
			.map(recordOf)
			.filter((record) => record.status !== 'closed');
		await Promise.all(records.map((record) => this.rewatch(record, { unwatchFirst: false })));
	}

	dispose(): void {
		if (this.#disposed) return;
		this.#disposed = true;
		for (const handle of this.#watches.values()) {
			const record = recordOf(handle);
			record.generation += 1;
			record.status = 'closed';
		}
		this.#watches.clear();
	}

	private throwIfDisposed(): void {
		if (this.#disposed) throw new Error('State store client is disposed.');
	}

	private allocateWatchId(): string {
		const watchId = `ui-state-watch-${this.#nextWatchNumber}`;
		this.#nextWatchNumber += 1;
		return watchId;
	}

	private async requestSnapshot(
		namespace: string,
		keyPrefix: string,
		watchId: string,
		maxEntries: number
	): Promise<StateWatchSnapshot> {
		const result = await this.#sendRequest(
			stateStoreCommand({
				case: 'watch',
				value: create(WatchStateSchema, { watchId, namespace, keyPrefix })
			})
		);
		if (result.case !== 'stateStoreResult' || result.value.result.case !== 'watch') {
			throw new Error('Server returned an unexpected state store watch response.');
		}
		const snapshot = result.value.result.value;
		if (snapshot.watchId !== watchId || snapshot.namespace !== namespace) {
			throw new Error('Server returned a state snapshot for an unexpected watch.');
		}
		if (snapshot.entries.length > maxEntries) {
			throw new Error('State snapshot exceeds the watch entry limit.');
		}
		for (const entry of snapshot.entries) {
			if (entry.namespace !== namespace || !entry.key.startsWith(keyPrefix)) {
				throw new Error('State snapshot contains an entry outside the watched scope.');
			}
		}
		return snapshot;
	}

	private async acknowledge(record: WatchRecord, appliedSequence: bigint): Promise<void> {
		try {
			await this.#sendRequest(
				stateStoreCommand({
					case: 'watchAck',
					value: create(WatchStateAckSchema, { watchId: record.watchId, appliedSequence })
				})
			);
		} catch (error) {
			if (
				error instanceof StateStoreRequestError &&
				error.code === StateStoreErrorCode.WATCH_NOT_FOUND
			) {
				await this.rewatch(record, { unwatchFirst: false });
			}
		}
	}

	private async rewatch(record: WatchRecord, options: { unwatchFirst: boolean }): Promise<void> {
		if (this.#disposed || record.status === 'closed' || record.status === 'rewatching') return;
		const previousWatchId = record.watchId;
		record.generation += 1;
		record.status = 'rewatching';
		const generation = record.generation;
		if (options.unwatchFirst) await this.bestEffortUnwatch(previousWatchId);
		for (let attempt = 0; attempt < maxRewatchAttempts; attempt += 1) {
			if (this.isStale(record, generation)) return;
			const watchId = this.allocateWatchId();
			let snapshot: StateWatchSnapshot;
			try {
				snapshot = await this.requestSnapshot(
					record.namespace,
					record.keyPrefix,
					watchId,
					record.maxEntries
				);
			} catch (error) {
				if (this.isStale(record, generation)) return;
				record.lastError = error;
				continue;
			}
			if (this.isStale(record, generation)) return;
			const handle = handleOf(record);
			if (this.#watches.get(previousWatchId) === handle) {
				this.#watches.delete(previousWatchId);
			}
			installSnapshot(record, snapshot, watchId);
			this.#watches.set(watchId, handle);
			return;
		}
		if (this.isStale(record, generation)) return;
		record.status = 'failed';
	}

	private isStale(record: WatchRecord, generation: number): boolean {
		return this.#disposed || record.generation !== generation || record.status === 'closed';
	}

	private async bestEffortUnwatch(watchId: string): Promise<void> {
		try {
			await this.#sendRequest(
				stateStoreCommand({ case: 'unwatch', value: create(UnwatchStateSchema, { watchId }) })
			);
		} catch {
			return;
		}
	}

	private async closeRecord(record: WatchRecord): Promise<void> {
		record.generation += 1;
		record.status = 'closed';
		const handle = handleOf(record);
		if (this.#watches.get(record.watchId) === handle) this.#watches.delete(record.watchId);
		if (this.#disposed) return;
		try {
			await this.#sendRequest(
				stateStoreCommand({
					case: 'unwatch',
					value: create(UnwatchStateSchema, { watchId: record.watchId })
				})
			);
		} catch (error) {
			if (
				error instanceof StateStoreRequestError &&
				error.code === StateStoreErrorCode.WATCH_NOT_FOUND
			) {
				return;
			}
			throw error;
		}
	}
}

const handleRecords = new WeakMap<StateStoreWatch, WatchRecord>();

function trackedHandle(record: WatchRecord, close: () => Promise<void>): StateStoreWatch {
	const handle = new StateStoreWatch(record, close);
	record.handle = handle;
	handleRecords.set(handle, record);
	return handle;
}

function recordOf(handle: StateStoreWatch): WatchRecord {
	const record = handleRecords.get(handle);
	if (!record) throw new Error('State store watch is not registered.');
	return record;
}

function handleOf(record: WatchRecord): StateStoreWatch {
	if (!record.handle) throw new Error('State store watch is not registered.');
	return record.handle;
}

function stateStoreCommand(action: StateStoreCommand['action']): Request['command'] {
	return {
		case: 'stateStoreCommand',
		value: create(StateStoreCommandSchema, { action })
	};
}

function throwIfKeyInvalid(namespace: string, key: string): void {
	if (namespace.length === 0 || key.length === 0) {
		throw new Error('State store operation requires a namespace and key.');
	}
}

function isApplicableUpdate(record: WatchRecord, update: StateStoreWatchUpdate): boolean {
	if (update.kind === StateStoreUpdateKind.PUT) {
		const entry = update.entry;
		return entry !== undefined && entry.namespace === record.namespace && entry.key === update.key;
	}
	return update.kind === StateStoreUpdateKind.DELETE || update.kind === StateStoreUpdateKind.EXPIRE;
}

function applyUpdate(record: WatchRecord, update: StateStoreWatchUpdate): void {
	if (update.kind === StateStoreUpdateKind.PUT && update.entry !== undefined) {
		record.entries.set(update.key, update.entry);
	} else {
		record.entries.delete(update.key);
	}
	record.appliedSequence = update.watchSequence;
	notifyListener(record);
}

function installSnapshot(
	record: WatchRecord,
	snapshot: StateWatchSnapshot,
	watchId?: string
): void {
	if (watchId !== undefined) record.watchId = watchId;
	const entries = new Map<string, StateEntry>();
	for (const entry of snapshot.entries) entries.set(entry.key, entry);
	record.entries = entries;
	record.snapshotRevision = snapshot.snapshotRevision;
	record.appliedSequence = 0n;
	record.status = 'active';
	record.lastError = null;
	notifyListener(record);
}

function notifyListener(record: WatchRecord): void {
	record.listener?.([...record.entries.values()]);
}
