import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import { ControlClientStateStore, type StateStoreWatch } from './control-client-state-store';
import { StateStoreRequestError } from './state-store-error';
import {
	StateEntrySchema,
	StateStoreErrorCode,
	StateStoreResultSchema,
	StateStoreUpdateKind,
	StateStoreWatchCloseReason,
	StateStoreWatchClosedSchema,
	StateStoreWatchUpdateSchema,
	type Ok,
	type Request,
	type StateEntry,
	type StateStoreCommand,
	type StateStoreWatchClosed,
	type StateStoreWatchUpdate
} from './proto/webrtc_pb';

const namespace = 'service/transcoder';
const schema = 'keeppeek.media-intent.v1';

type Command = Request['command'];
type Result = NonNullable<Ok['result']>;

function testEntry(key: string, revision: bigint): StateEntry {
	return create(StateEntrySchema, {
		namespace,
		key,
		schema,
		value: { desired: true, key },
		revision,
		ownerId: 'server'
	});
}

function snapshotResult(watchId: string, entries: StateEntry[], snapshotRevision = 10n): Result {
	return {
		case: 'stateStoreResult',
		value: create(StateStoreResultSchema, {
			result: {
				case: 'watch',
				value: { watchId, namespace, keyPrefix: '', snapshotRevision, entries }
			}
		})
	};
}

function ackResult(watchId: string, appliedSequence: bigint): Result {
	return {
		case: 'stateStoreResult',
		value: create(StateStoreResultSchema, {
			result: { case: 'watchAck', value: { watchId, appliedSequence } }
		})
	};
}

function unwatchResult(watchId: string): Result {
	return {
		case: 'stateStoreResult',
		value: create(StateStoreResultSchema, {
			result: { case: 'unwatched', value: { watchId } }
		})
	};
}

function entryResult(entry: StateEntry): Result {
	return {
		case: 'stateStoreResult',
		value: create(StateStoreResultSchema, { result: { case: 'entry', value: entry } })
	};
}

function testUpdate(
	watchId: string,
	key: string,
	kind: StateStoreUpdateKind,
	watchSequence: bigint,
	options: { revision?: bigint; entry?: StateEntry; namespace?: string } = {}
): StateStoreWatchUpdate {
	return create(StateStoreWatchUpdateSchema, {
		watchId,
		namespace: options.namespace ?? namespace,
		key,
		revision: options.revision ?? 11n,
		kind,
		entry: options.entry,
		watchSequence
	});
}

function testClosed(watchId: string): StateStoreWatchClosed {
	return create(StateStoreWatchClosedSchema, {
		watchId,
		namespace,
		reason: StateStoreWatchCloseReason.ACK_TIMEOUT
	});
}

function storeAction(command: Command): StateStoreCommand['action'] {
	if (command.case !== 'stateStoreCommand') throw new Error('Expected a state store command.');
	return command.value.action;
}

function ackSequenceOf(command: Command): bigint {
	const action = storeAction(command);
	if (action.case !== 'watchAck') throw new Error('Expected a watch acknowledgement.');
	return action.value.appliedSequence;
}

type TransportBehavior = (action: StateStoreCommand['action']) => Promise<Result> | Result;

class FakeTransport {
	readonly sent: Command[] = [];
	constructor(private readonly behavior: TransportBehavior) {}

	async send(command: Command): Promise<Result> {
		this.sent.push(command);
		return this.behavior(storeAction(command));
	}

	acks(): bigint[] {
		return this.sent
			.filter((command) => {
				try {
					return storeAction(command).case === 'watchAck';
				} catch {
					return false;
				}
			})
			.map(ackSequenceOf);
	}

	unwatchedIds(): string[] {
		return this.sent
			.filter((command) => {
				try {
					return storeAction(command).case === 'unwatch';
				} catch {
					return false;
				}
			})
			.map((command) => {
				const action = storeAction(command);
				if (action.case !== 'unwatch') throw new Error('Expected an unwatch command.');
				return action.value.watchId;
			});
	}
}

function entryMap(watch: StateStoreWatch): Map<string, bigint> {
	return new Map(watch.entries().map((entry) => [entry.key, entry.revision]));
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

async function flush(): Promise<void> {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('ControlClientStateStore', () => {
	it('converges two client mirrors on the same snapshot and updates', async () => {
		const initial = [testEntry('intent/a', 10n), testEntry('intent/b', 10n)];
		const makeTransport = () =>
			new FakeTransport((action) => {
				if (action.case === 'watch') return snapshotResult(action.value.watchId, initial);
				if (action.case === 'watchAck')
					return ackResult(action.value.watchId, action.value.appliedSequence);
				return unwatchResult('unused');
			});
		const transports = [makeTransport(), makeTransport()];
		const clients = transports.map(
			(transport) => new ControlClientStateStore((command) => transport.send(command))
		);
		const mirrors: StateStoreWatch[] = [];
		for (const client of clients) mirrors.push(await client.watch(namespace));

		const updates = [
			{ key: 'intent/c', kind: StateStoreUpdateKind.PUT, entry: testEntry('intent/c', 11n) },
			{ key: 'intent/a', kind: StateStoreUpdateKind.DELETE },
			{ key: 'intent/b', kind: StateStoreUpdateKind.EXPIRE }
		];
		for (const [index, client] of clients.entries()) {
			const watch = mirrors[index]!;
			for (const [sequence, update] of updates.entries()) {
				await client.handleWatchUpdate(
					testUpdate(watch.watchId, update.key, update.kind, BigInt(sequence + 1), {
						entry: update.entry
					})
				);
			}
		}

		expect(entryMap(mirrors[0]!)).toEqual(entryMap(mirrors[1]!));
		expect(entryMap(mirrors[0]!)).toEqual(new Map([['intent/c', 11n]]));
		expect(mirrors[0]!.appliedSequence).toBe(3n);
		expect(transports[0]!.acks()).toEqual([1n, 2n, 3n]);
		expect(transports[1]!.acks()).toEqual([1n, 2n, 3n]);
	});

	it('excludes updates from other namespaces without acknowledging them', async () => {
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch')
				return snapshotResult(action.value.watchId, [testEntry('intent/a', 10n)]);
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult('unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace);

		await client.handleWatchUpdate(
			testUpdate(watch.watchId, 'intent/a', StateStoreUpdateKind.PUT, 1n, {
				namespace: 'service/other',
				entry: testEntry('intent/a', 11n)
			})
		);

		expect(entryMap(watch)).toEqual(new Map([['intent/a', 10n]]));
		expect(watch.appliedSequence).toBe(0n);
		expect(watch.status).toBe('active');
		expect(transport.acks()).toEqual([]);

		await client.handleWatchUpdate(
			testUpdate(watch.watchId, 'intent/a', StateStoreUpdateKind.PUT, 1n, {
				entry: testEntry('intent/a', 11n)
			})
		);
		expect(entryMap(watch)).toEqual(new Map([['intent/a', 11n]]));
		expect(transport.acks()).toEqual([1n]);
	});

	it('surfaces schema failures without retrying the write', async () => {
		const transport = new FakeTransport(() => {
			throw new StateStoreRequestError('Schema is not allowed here.', {
				namespace,
				key: 'intent/a',
				code: StateStoreErrorCode.SCHEMA_INVALID
			});
		});
		const client = new ControlClientStateStore((command) => transport.send(command));

		await expect(
			client.put(namespace, 'intent/a', { schema: 'bogus.v1', value: { desired: true } })
		).rejects.toMatchObject({ code: StateStoreErrorCode.SCHEMA_INVALID });
		expect(transport.sent).toHaveLength(1);
	});

	it('replaces stale cached keys with the fresh snapshot after a gap', async () => {
		const snapshots = [
			[testEntry('intent/a', 10n), testEntry('intent/stale', 10n)],
			[testEntry('intent/a', 12n), testEntry('intent/c', 12n)]
		];
		let calls = 0;
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch') {
				const snapshot = snapshots[Math.min(calls, snapshots.length - 1)]!;
				calls += 1;
				return snapshotResult(action.value.watchId, snapshot, 10n + BigInt(calls));
			}
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult(action.case === 'unwatch' ? action.value.watchId : 'unused');
		});
		const seen: string[][] = [];
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace, {
			listener: (entries) => seen.push(entries.map((entry) => entry.key).sort())
		});
		const previousWatchId = watch.watchId;

		await client.handleWatchUpdate(
			testUpdate(watch.watchId, 'intent/a', StateStoreUpdateKind.PUT, 1n, {
				entry: testEntry('intent/a', 11n)
			})
		);
		expect(watch.get('intent/a')?.revision).toBe(11n);

		await client.handleWatchUpdate(
			testUpdate(previousWatchId, 'intent/a', StateStoreUpdateKind.PUT, 3n, {
				entry: testEntry('intent/a', 13n)
			})
		);

		expect(watch.watchId).not.toBe(previousWatchId);
		expect(watch.status).toBe('active');
		expect(watch.appliedSequence).toBe(0n);
		expect(entryMap(watch)).toEqual(
			new Map([
				['intent/a', 12n],
				['intent/c', 12n]
			])
		);
		const kinds = transport.sent.map((command) => storeAction(command).case);
		expect(kinds).toEqual(['watch', 'watchAck', 'unwatch', 'watch']);
		expect(seen.at(-1)).toEqual(['intent/a', 'intent/c']);

		await client.handleWatchUpdate(
			testUpdate(previousWatchId, 'intent/c', StateStoreUpdateKind.PUT, 1n, {
				entry: testEntry('intent/c', 14n)
			})
		);
		expect(entryMap(watch)).toEqual(
			new Map([
				['intent/a', 12n],
				['intent/c', 12n]
			])
		);
		expect(transport.sent).toHaveLength(4);
	});

	it('reconnects with fresh IDs after a terminal close and discards post-close updates', async () => {
		let calls = 0;
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch') {
				calls += 1;
				const entries = calls === 1 ? [testEntry('intent/a', 10n)] : [testEntry('intent/a', 20n)];
				return snapshotResult(action.value.watchId, entries, BigInt(10 * calls));
			}
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult('unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace);
		const previousWatchId = watch.watchId;

		await client.handleWatchClosed(testClosed(previousWatchId));

		expect(watch.watchId).not.toBe(previousWatchId);
		expect(watch.status).toBe('active');
		expect(entryMap(watch)).toEqual(new Map([['intent/a', 20n]]));
		expect(transport.sent.map((command) => storeAction(command).case)).toEqual(['watch', 'watch']);

		await client.handleWatchUpdate(
			testUpdate(previousWatchId, 'intent/a', StateStoreUpdateKind.PUT, 1n, {
				entry: testEntry('intent/a', 99n)
			})
		);
		expect(entryMap(watch)).toEqual(new Map([['intent/a', 20n]]));
		expect(transport.sent).toHaveLength(2);
	});

	it('ignores duplicate sequences and recovers from gapped sequences', async () => {
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch')
				return snapshotResult(action.value.watchId, [testEntry('intent/a', 10n)]);
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult(action.case === 'unwatch' ? action.value.watchId : 'unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace);
		const previousWatchId = watch.watchId;
		const update = testUpdate(watch.watchId, 'intent/b', StateStoreUpdateKind.PUT, 1n, {
			entry: testEntry('intent/b', 11n)
		});

		await client.handleWatchUpdate(update);
		await client.handleWatchUpdate(update);

		expect(entryMap(watch)).toEqual(
			new Map([
				['intent/a', 10n],
				['intent/b', 11n]
			])
		);
		expect(transport.acks()).toEqual([1n]);

		await client.handleWatchUpdate(
			testUpdate(previousWatchId, 'intent/c', StateStoreUpdateKind.PUT, 5n, {
				entry: testEntry('intent/c', 12n)
			})
		);

		expect(watch.watchId).not.toBe(previousWatchId);
		expect(watch.status).toBe('active');
		expect(transport.sent.map((command) => storeAction(command).case)).toEqual([
			'watch',
			'watchAck',
			'unwatch',
			'watch'
		]);
	});

	it('reconnects watches with fresh snapshots after a connection drop', async () => {
		let calls = 0;
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch') {
				calls += 1;
				const entries = calls === 1 ? [testEntry('intent/a', 10n)] : [testEntry('intent/b', 20n)];
				return snapshotResult(action.value.watchId, entries, BigInt(10 * calls));
			}
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult('unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace);
		const previousWatchId = watch.watchId;

		client.handleConnectionClosed();
		expect(watch.status).toBe('disconnected');
		expect(entryMap(watch)).toEqual(new Map([['intent/a', 10n]]));

		await client.handleConnectionOpened();

		expect(watch.watchId).not.toBe(previousWatchId);
		expect(watch.status).toBe('active');
		expect(entryMap(watch)).toEqual(new Map([['intent/b', 20n]]));
		expect(transport.sent.map((command) => storeAction(command).case)).toEqual(['watch', 'watch']);
	});

	it('cancels a reconnect in flight when disposed', async () => {
		const gate = deferred<Result>();
		let watchCalls = 0;
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch') {
				watchCalls += 1;
				if (watchCalls > 1) return gate.promise;
				return snapshotResult(action.value.watchId, [testEntry('intent/a', 10n)]);
			}
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult('unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace);
		const sentBeforeGap = transport.sent.length;

		const reconnect = client.handleWatchUpdate(
			testUpdate(watch.watchId, 'intent/a', StateStoreUpdateKind.PUT, 2n, {
				entry: testEntry('intent/a', 11n)
			})
		);
		await flush();
		client.dispose();
		gate.resolve(snapshotResult('orphan', [testEntry('intent/late', 99n)]));
		await reconnect;
		await flush();

		expect(watch.status).toBe('closed');
		expect(client.watchCount).toBe(0);
		expect(transport.sent.length).toBe(sentBeforeGap + 2);
		expect(storeAction(transport.sent.at(-1)!).case).toBe('watch');
		expect(watch.get('intent/late')).toBeUndefined();

		await client.handleWatchUpdate(
			testUpdate(watch.watchId, 'intent/a', StateStoreUpdateKind.PUT, 1n, {
				entry: testEntry('intent/a', 12n)
			})
		);
		expect(transport.sent.length).toBe(sentBeforeGap + 2);
	});

	it('retains the caller draft on revision conflicts without a blind overwrite', async () => {
		const transport = new FakeTransport((action) => {
			if (action.case === 'put') {
				throw new StateStoreRequestError('State revision does not match.', {
					namespace,
					key: 'intent/a',
					code: StateStoreErrorCode.CONFLICT,
					currentRevision: 9n
				});
			}
			return entryResult(testEntry('intent/a', 9n));
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const draft = { schema, value: { desired: true, key: 'intent/a' }, expectedRevision: 7n };
		const draftRevision = draft.expectedRevision;

		const failure = await client.put(namespace, 'intent/a', draft).then(
			() => null,
			(error: unknown) => error
		);

		expect(failure).toBeInstanceOf(StateStoreRequestError);
		expect(failure).toMatchObject({ code: StateStoreErrorCode.CONFLICT, currentRevision: 9n });
		expect(transport.sent).toHaveLength(1);
		expect(draft.expectedRevision).toBe(draftRevision);
		expect(draft.value).toEqual({ desired: true, key: 'intent/a' });
	});

	it('rejects a second concurrent watch while the first snapshot is pending', async () => {
		const gate = deferred<Result>();
		const requestedIds: string[] = [];
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch') {
				requestedIds.push(action.value.watchId);
				return gate.promise.then(() => snapshotResult(action.value.watchId, []));
			}
			return unwatchResult(action.case === 'unwatch' ? action.value.watchId : 'unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command), {
			maxWatches: 1
		});

		const first = client.watch(namespace);
		await expect(client.watch(namespace)).rejects.toThrow('State store watch limit reached.');
		expect(requestedIds).toHaveLength(1);

		gate.resolve(snapshotResult(requestedIds[0]!, []));
		const watch = await first;
		expect(watch.status).toBe('active');
		expect(client.watchCount).toBe(1);

		await watch.close();
		const replacement = await client.watch(namespace);
		expect(replacement.status).toBe('active');
		expect(client.watchCount).toBe(1);
	});

	it('recovers without acknowledging a live PUT that exceeds the entry limit', async () => {
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch')
				return snapshotResult(action.value.watchId, [testEntry('intent/a', 10n)]);
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult(action.case === 'unwatch' ? action.value.watchId : 'unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace, { maxEntries: 1 });
		const previousWatchId = watch.watchId;

		await client.handleWatchUpdate(
			testUpdate(watch.watchId, 'intent/b', StateStoreUpdateKind.PUT, 1n, {
				entry: testEntry('intent/b', 11n)
			})
		);

		expect(watch.watchId).not.toBe(previousWatchId);
		expect(watch.status).toBe('active');
		expect(entryMap(watch)).toEqual(new Map([['intent/a', 10n]]));
		expect(transport.acks()).toEqual([]);
		expect(transport.unwatchedIds()).toEqual([previousWatchId]);
	});

	it('applies a live PUT that replaces an existing key at the entry limit', async () => {
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch')
				return snapshotResult(action.value.watchId, [testEntry('intent/a', 10n)]);
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult(action.case === 'unwatch' ? action.value.watchId : 'unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace, { maxEntries: 1 });

		await client.handleWatchUpdate(
			testUpdate(watch.watchId, 'intent/a', StateStoreUpdateKind.PUT, 1n, {
				entry: testEntry('intent/a', 11n)
			})
		);

		expect(entryMap(watch)).toEqual(new Map([['intent/a', 11n]]));
		expect(watch.status).toBe('active');
		expect(transport.acks()).toEqual([1n]);
		expect(transport.unwatchedIds()).toEqual([]);
	});

	it('unwatches the server registration when the snapshot exceeds the entry limit', async () => {
		const requestedIds: string[] = [];
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch') {
				requestedIds.push(action.value.watchId);
				return snapshotResult(action.value.watchId, [
					testEntry('intent/a', 10n),
					testEntry('intent/b', 10n)
				]);
			}
			return unwatchResult(action.case === 'unwatch' ? action.value.watchId : 'unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));

		await expect(client.watch(namespace, { maxEntries: 1 })).rejects.toThrow(
			'State snapshot exceeds the watch entry limit.'
		);
		expect(requestedIds).toHaveLength(1);
		expect(transport.unwatchedIds()).toEqual(requestedIds);
		expect(client.watchCount).toBe(0);
	});

	it('unwatches the pending rewatch registration when the watch closes mid-flight', async () => {
		const gate = deferred<Result>();
		const requestedIds: string[] = [];
		const transport = new FakeTransport((action) => {
			if (action.case === 'watch') {
				requestedIds.push(action.value.watchId);
				if (requestedIds.length > 1) return gate.promise;
				return snapshotResult(action.value.watchId, [testEntry('intent/a', 10n)]);
			}
			if (action.case === 'watchAck')
				return ackResult(action.value.watchId, action.value.appliedSequence);
			return unwatchResult(action.case === 'unwatch' ? action.value.watchId : 'unused');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));
		const watch = await client.watch(namespace);

		const rewatch = client.handleWatchUpdate(
			testUpdate(watch.watchId, 'intent/a', StateStoreUpdateKind.PUT, 2n, {
				entry: testEntry('intent/a', 11n)
			})
		);
		await flush();
		expect(requestedIds).toHaveLength(2);

		await watch.close();
		gate.resolve(snapshotResult(requestedIds[1]!, [testEntry('intent/a', 12n)]));
		await rewatch;
		await flush();

		expect(client.watchCount).toBe(0);
		expect(transport.unwatchedIds()).toEqual(expect.arrayContaining(requestedIds));
		expect(transport.unwatchedIds()).toContain(requestedIds[1]);
		expect(watch.get('intent/a')?.revision).toBe(10n);
	});

	it('reads and deletes single keys through typed responses', async () => {
		const transport = new FakeTransport((action) => {
			if (action.case === 'get') return entryResult(testEntry(action.value.key, 4n));
			if (action.case === 'delete') {
				return {
					case: 'stateStoreResult',
					value: create(StateStoreResultSchema, {
						result: {
							case: 'deleted',
							value: { namespace: action.value.namespace, key: action.value.key, revision: 5n }
						}
					})
				};
			}
			throw new Error('Unexpected command.');
		});
		const client = new ControlClientStateStore((command) => transport.send(command));

		await expect(client.get(namespace, 'intent/a')).resolves.toMatchObject({
			key: 'intent/a',
			revision: 4n
		});
		await expect(client.delete(namespace, 'intent/a', 4n)).resolves.toMatchObject({
			key: 'intent/a',
			revision: 5n
		});
	});
});
