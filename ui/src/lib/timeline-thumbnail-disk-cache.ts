const DATABASE_NAME = 'keeppeek-timeline-v1';
const STORE_NAME = 'thumbnails';
const MAX_DISK_BYTES = 128 * 1_048_576;

export type TimelineThumbnailIdentity = {
	sourceId: string;
	eventId: string;
	revision: number;
	attachmentId: string;
	sizeClass: number;
};

type ThumbnailRecord = {
	key: string;
	blob: Blob;
	byteLength: number;
	lastAccessMs: number;
};

export class TimelineThumbnailDiskCache {
	#operation: Promise<unknown> = Promise.resolve();

	get(identity: TimelineThumbnailIdentity): Promise<Blob | null> {
		return this.#enqueue(async () => {
			const database = await openDatabase();
			if (!database) return null;
			try {
				const key = timelineThumbnailDiskKey(identity);
				const record = await requestResult<ThumbnailRecord | undefined>(
					database.transaction(STORE_NAME).objectStore(STORE_NAME).get(key)
				);
				if (!record) return null;
				record.lastAccessMs = Date.now();
				await transactionComplete(database.transaction(STORE_NAME, 'readwrite'), (transaction) =>
					transaction.objectStore(STORE_NAME).put(record)
				);
				return record.blob;
			} finally {
				database.close();
			}
		});
	}

	put(identity: TimelineThumbnailIdentity, blob: Blob): Promise<void> {
		return this.#enqueue(async () => {
			if (blob.size === 0 || blob.size > MAX_DISK_BYTES) return;
			const database = await openDatabase();
			if (!database) return;
			try {
				const record: ThumbnailRecord = {
					key: timelineThumbnailDiskKey(identity),
					blob,
					byteLength: blob.size,
					lastAccessMs: Date.now()
				};
				await transactionComplete(database.transaction(STORE_NAME, 'readwrite'), (transaction) =>
					transaction.objectStore(STORE_NAME).put(record)
				);
				await evictOverflow(database);
			} finally {
				database.close();
			}
		});
	}

	clear(): Promise<void> {
		return this.#enqueue(async () => {
			const database = await openDatabase();
			if (!database) return;
			try {
				await transactionComplete(database.transaction(STORE_NAME, 'readwrite'), (transaction) =>
					transaction.objectStore(STORE_NAME).clear()
				);
			} finally {
				database.close();
			}
		});
	}

	#enqueue<T>(operation: () => Promise<T>): Promise<T> {
		const result = this.#operation.catch(() => undefined).then(operation);
		this.#operation = result;
		return result;
	}
}

export function timelineThumbnailDiskKey(identity: TimelineThumbnailIdentity): string {
	return [
		identity.sourceId,
		identity.eventId,
		identity.revision,
		identity.attachmentId,
		identity.sizeClass
	]
		.map((part) => encodeURIComponent(String(part)))
		.join(':');
}

export function thumbnailEvictionKeys(
	records: readonly Pick<ThumbnailRecord, 'key' | 'byteLength' | 'lastAccessMs'>[],
	maxBytes = MAX_DISK_BYTES
): string[] {
	let totalBytes = records.reduce((total, record) => total + record.byteLength, 0);
	const keys: string[] = [];
	for (const record of records.toSorted((left, right) => left.lastAccessMs - right.lastAccessMs)) {
		if (totalBytes <= maxBytes) break;
		totalBytes -= record.byteLength;
		keys.push(record.key);
	}
	return keys;
}

async function evictOverflow(database: IDBDatabase): Promise<void> {
	const records = await requestResult<ThumbnailRecord[]>(
		database.transaction(STORE_NAME).objectStore(STORE_NAME).getAll()
	);
	const keys = thumbnailEvictionKeys(records);
	if (keys.length === 0) return;
	await transactionComplete(database.transaction(STORE_NAME, 'readwrite'), (transaction) => {
		const store = transaction.objectStore(STORE_NAME);
		for (const key of keys) store.delete(key);
	});
}

function openDatabase(): Promise<IDBDatabase | null> {
	if (typeof indexedDB === 'undefined') return Promise.resolve(null);
	return new Promise((resolve, reject) => {
		const request = indexedDB.open(DATABASE_NAME, 1);
		request.onupgradeneeded = () => {
			if (!request.result.objectStoreNames.contains(STORE_NAME)) {
				request.result.createObjectStore(STORE_NAME, { keyPath: 'key' });
			}
		};
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error ?? new Error('Unable to open thumbnail cache.'));
	});
}

function requestResult<T>(request: IDBRequest<T>): Promise<T> {
	return new Promise((resolve, reject) => {
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error ?? new Error('Thumbnail cache request failed.'));
	});
}

function transactionComplete(
	transaction: IDBTransaction,
	operation: (transaction: IDBTransaction) => IDBRequest | void
): Promise<void> {
	return new Promise((resolve, reject) => {
		operation(transaction);
		transaction.oncomplete = () => resolve();
		transaction.onerror = () =>
			reject(transaction.error ?? new Error('Thumbnail cache transaction failed.'));
		transaction.onabort = () =>
			reject(transaction.error ?? new Error('Thumbnail cache transaction was aborted.'));
	});
}
