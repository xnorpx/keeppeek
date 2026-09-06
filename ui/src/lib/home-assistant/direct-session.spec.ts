import { create } from '@bufbuild/protobuf';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ServerCapabilitiesSchema } from '../proto/webrtc_pb';
import type { DirectPeer } from './direct-peer';
import { DirectSession } from './direct-session';
import { CardConnectionError } from './direct-http';
import type { SessionSnapshot } from './connection-manager';

const front = { source_id: 'front-door', quality: 'auto' as const };
const driveway = { source_id: 'driveway', quality: 'auto' as const };

function capabilities(sourceIds = ['front-door', 'driveway']) {
	return create(ServerCapabilitiesSchema, {
		revision: 1n,
		cameras: ['front-door', 'driveway', 'offline'].map((sourceId) => ({
			sourceId,
			displayName: `Camera ${sourceId}`
		})),
		sourceSessions: sourceIds.map((sourceId) => ({
			sourceId,
			sourceSessionId: `session-${sourceId}`,
			displayName: sourceId,
			video: { variants: [{ variantId: 'main' }] }
		}))
	});
}

function harness() {
	const peers: Array<Pick<DirectPeer, 'open' | 'close' | 'subscribe' | 'unsubscribe' | 'stream'>> =
		[];
	const callbacks: Array<ConstructorParameters<typeof DirectPeer>[1]> = [];
	const updates: SessionSnapshot[] = [];
	const factory = vi.fn((events: ConstructorParameters<typeof DirectPeer>[1]) => {
		const peer = {
			open: vi.fn(async () => {
				const value = capabilities();
				events.capabilities(value);
				return value;
			}),
			close: vi.fn(async () => undefined),
			subscribe: vi.fn(async ({ subscriptionId }: { subscriptionId: string }) => ({
				mid: subscriptionId,
				variantId: 'main'
			})),
			unsubscribe: vi.fn(async () => undefined),
			stream: vi.fn(() => null)
		};
		peers.push(peer);
		callbacks.push(events);
		return peer;
	});
	const session = new DirectSession(
		{ endpoint: 'https://keeppeek.example.net', token: 'test-credential' },
		(snapshot) => updates.push(snapshot),
		factory
	);
	return { session, peers, callbacks, updates, factory };
}

afterEach(() => vi.useRealTimers());

describe('Home Assistant live session reconciliation', () => {
	it('does not duplicate subscriptions when cards or capability snapshots are refreshed', async () => {
		const { session, peers, callbacks } = harness();
		session.configure([front, driveway]);
		await vi.waitFor(() => expect(peers[0]?.subscribe).toHaveBeenCalledTimes(2));
		session.configure([front, driveway]);
		callbacks[0]!.capabilities(capabilities());
		await vi.waitFor(() => expect(peers[0]!.subscribe).toHaveBeenCalledTimes(2));
		session.configure([driveway]);
		await vi.waitFor(() => expect(peers[0]!.unsubscribe).toHaveBeenCalledTimes(1));
		expect(peers[0]!.close).not.toHaveBeenCalled();
		await session.close();
	});

	it('keeps a working camera live while reporting offline and unknown IDs separately', async () => {
		const { session, updates, peers } = harness();
		session.configure([
			front,
			{ ...front, source_id: 'offline' },
			{ ...front, source_id: 'missing' }
		]);
		await vi.waitFor(() => expect(updates.at(-1)?.streams.size).toBe(3));
		expect(peers[0]!.subscribe).toHaveBeenCalledTimes(1);
		expect(updates.at(-1)!.cameras.find((camera) => camera.source_id === 'front-door')?.title).toBe(
			'Camera front-door'
		);
		const states = [...updates.at(-1)!.streams.values()];
		expect(states[1]!.message).toMatch(/offline/i);
		expect(states[2]!.message).toMatch(/source ID/i);
		await session.close();
	});

	it('rebuilds only current subscriptions after an interrupted session', async () => {
		vi.useFakeTimers();
		const { session, peers, callbacks, factory } = harness();
		session.configure([front, driveway]);
		await vi.advanceTimersByTimeAsync(0);
		callbacks[0]!.failure(new CardConnectionError('network', 'Connection interrupted.'));
		session.configure([driveway]);
		await vi.advanceTimersByTimeAsync(1001);
		expect(factory).toHaveBeenCalledTimes(2);
		expect(peers[0]!.close).toHaveBeenCalledTimes(1);
		expect(peers[1]!.subscribe).toHaveBeenCalledTimes(1);
		expect(peers[1]!.subscribe).toHaveBeenCalledWith(
			expect.objectContaining({ sourceSessionId: 'session-driveway' })
		);
		await session.close();
	});

	it('does not automatically retry authentication failures', async () => {
		vi.useFakeTimers();
		const { session, callbacks, factory, updates } = harness();
		session.configure([front]);
		await vi.advanceTimersByTimeAsync(0);
		callbacks[0]!.failure(new CardConnectionError('authentication', 'Replace the access key.'));
		await vi.advanceTimersByTimeAsync(120_000);
		expect(factory).toHaveBeenCalledTimes(1);
		expect(updates.at(-1)?.status).toBe('error');
		await session.close();
	});

	it('coalesces manual retries during cleanup and reconnects without the old backoff', async () => {
		vi.useFakeTimers();
		const { session, peers, callbacks, factory } = harness();
		session.configure([front]);
		await vi.advanceTimersByTimeAsync(0);
		let finishClose!: () => void;
		vi.mocked(peers[0]!.close).mockReturnValueOnce(
			new Promise<void>((resolve) => {
				finishClose = resolve;
			})
		);
		callbacks[0]!.failure(new CardConnectionError('network', 'Connection interrupted.'));
		session.configure([driveway]);
		session.retry();
		session.retry();
		await vi.advanceTimersByTimeAsync(0);
		expect(factory).toHaveBeenCalledTimes(1);
		expect(peers[0]!.close).toHaveBeenCalledTimes(1);
		finishClose();
		await vi.advanceTimersByTimeAsync(1);
		expect(factory).toHaveBeenCalledTimes(2);
		expect(peers[1]!.subscribe).toHaveBeenCalledTimes(1);
		expect(peers[1]!.subscribe).toHaveBeenCalledWith(
			expect.objectContaining({ sourceSessionId: 'session-driveway' })
		);
		await session.close();
	});

	it('does not allocate a connection for a card removed before its queued work', async () => {
		const { session, factory } = harness();
		session.configure([front]);
		await session.close();
		expect(factory).not.toHaveBeenCalled();
	});
});
