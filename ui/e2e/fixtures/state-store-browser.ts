import type { ControlClientStateStore } from '../../src/lib/control-client-state-store';
import { StateStoreWatchCloseReason } from '../../src/lib/proto/webrtc_pb';

const schema = 'keeppeek.media-intent.v1';
const value = { role: 'subscribe', desired: true, source_id: 'test', media_kind: 'video' };

async function client() {
	const modulePath = '/src/lib/control-client.ts';
	const loaded = (await import(/* @vite-ignore */ modulePath)) as {
		ControlClient: new () => {
			stateStore: ControlClientStateStore;
			getServerCapabilities(): Promise<{ capabilityIds: string[] }>;
			close(): Promise<void>;
		};
	};
	return new loaded.ControlClient();
}

async function until(predicate: () => boolean, timeoutMs = 10_000): Promise<void> {
	const deadline = performance.now() + timeoutMs;
	while (!predicate()) {
		if (performance.now() >= deadline) throw new Error('State-store convergence timed out.');
		await new Promise((resolve) => setTimeout(resolve, 20));
	}
}

export async function convergenceAndRecovery() {
	const writer = await client();
	const reader = await client();
	const namespace = `service/e2e-${crypto.randomUUID()}/`;
	try {
		const capabilities = await writer.getServerCapabilities();
		if (!capabilities.capabilityIds.includes('keeppeek.state-store.v1')) {
			throw new Error('Durable state-store capability is missing.');
		}
		const first = await writer.stateStore.put(namespace, 'watched/key', {
			schema,
			value,
			expectedRevision: 0n
		});
		const mirror = await reader.stateStore.watch(namespace, { keyPrefix: 'watched/' });
		const initialRevision = mirror.get('watched/key')?.revision;
		let revision = first.revision;
		for (let index = 0; index < 20; index += 1) {
			await writer.stateStore.put(namespace, 'unrelated/key', { schema, value });
			revision = (
				await writer.stateStore.put(namespace, 'watched/key', {
					schema,
					value,
					expectedRevision: revision
				})
			).revision;
		}
		await until(() => mirror.get('watched/key')?.revision === revision);
		const sequence = mirror.appliedSequence;
		const oldWatchId = mirror.watchId;
		await reader.close();
		await writer.stateStore.delete(namespace, 'watched/key', revision);
		const replacement = await writer.stateStore.put(namespace, 'watched/replacement', {
			schema,
			value,
			expectedRevision: 0n
		});
		await reader.getServerCapabilities();
		await until(() => mirror.status === 'active' && mirror.watchId !== oldWatchId);
		const recoveredKeys = mirror.entries().map((entry) => entry.key);
		const recoveredRevision = mirror.get('watched/replacement')?.revision;
		await mirror.close();
		return {
			initialRevision: String(initialRevision),
			sequence: String(sequence),
			recoveredKeys,
			recoveredRevision: String(recoveredRevision),
			expectedRevision: String(replacement.revision),
			status: mirror.status,
			watchCount: reader.stateStore.watchCount
		};
	} finally {
		await Promise.all([writer.close(), reader.close()]);
	}
}

export async function stalledWatchCloses() {
	const writer = await client();
	const reader = await client();
	const namespace = `service/stalled-${crypto.randomUUID()}/`;
	let closeReason: StateStoreWatchCloseReason | undefined;
	let watchId = '';
	// Drop application delivery to exercise the real server acknowledgement deadline.
	reader.stateStore.handleWatchUpdate = async () => {};
	reader.stateStore.handleWatchClosed = async (closed) => {
		if (closed.watchId === watchId) closeReason = closed.reason;
	};
	try {
		watchId = (await reader.stateStore.watch(namespace)).watchId;
		await writer.stateStore.put(namespace, 'key', { schema, value });
		await until(() => closeReason !== undefined, 40_000);
		return { closeReason, expected: StateStoreWatchCloseReason.ACK_TIMEOUT };
	} finally {
		await Promise.all([writer.close(), reader.close()]);
	}
}
