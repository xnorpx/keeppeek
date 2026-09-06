import { describe, expect, it, vi } from 'vitest';
import { parseCardConfig } from './config';
import {
	KeepPeekConnectionManager,
	type SessionAdapter,
	type SessionSnapshot
} from './connection-manager';

function harness() {
	const sessions: SessionAdapter[] = [];
	const notifications: Array<(snapshot: SessionSnapshot) => void> = [];
	const factory = vi.fn((_config, notify: (snapshot: SessionSnapshot) => void) => {
		const session = {
			configure: vi.fn(),
			close: vi.fn(async () => undefined),
			retry: vi.fn()
		};
		sessions.push(session);
		notifications.push(notify);
		return session;
	});
	return { manager: new KeepPeekConnectionManager(factory), factory, sessions, notifications };
}

function config(sourceId = 'front-door', token = 'test-only-credential') {
	return parseCardConfig({
		type: 'custom:keeppeek-card',
		endpoint: 'https://keeppeek.example.net',
		token,
		sources: [{ source_id: sourceId }]
	});
}

describe('Home Assistant connection ownership', () => {
	it('continues notifying consumers when another consumer releases inside its callback', async () => {
		const { manager, notifications } = harness();
		const first = await manager.acquire(config(), vi.fn());
		const second = await manager.acquire(config(), (snapshot) => {
			if (snapshot.status === 'ready') void second.release();
		});
		const thirdListener = vi.fn();
		const third = await manager.acquire(config(), thirdListener);
		const snapshot: SessionSnapshot = {
			status: 'ready',
			message: null,
			cameras: [],
			streams: new Map()
		};
		notifications[0]!(snapshot);
		expect(thirdListener).toHaveBeenLastCalledWith(snapshot);
		await first.release();
		await third.release();
	});

	it('updates a cards source selection without closing its shared session', async () => {
		const { manager, factory, sessions } = harness();
		const lease = await manager.acquire(config(), vi.fn());
		lease.updateSources(config('driveway').sources);
		expect(factory).toHaveBeenCalledTimes(1);
		expect(sessions[0]!.configure).toHaveBeenLastCalledWith(config('driveway').sources);
		expect(sessions[0]!.close).not.toHaveBeenCalled();
		await lease.release();
	});

	it('shares a concurrently acquired connection and deduplicates media subscriptions', async () => {
		const { manager, factory, sessions } = harness();
		const cardConfig = config();
		const leases = await Promise.all(
			Array.from({ length: 3 }, () => manager.acquire(cardConfig, vi.fn()))
		);
		expect(factory).toHaveBeenCalledTimes(1);
		expect(sessions[0]!.configure).toHaveBeenLastCalledWith(cardConfig.sources);
		await leases[0]!.release();
		await leases[0]!.release();
		await leases[1]!.release();
		expect(sessions[0]!.close).not.toHaveBeenCalled();
		await leases[2]!.release();
		expect(sessions[0]!.close).toHaveBeenCalledTimes(1);
	});

	it('releases only the removed cards sources', async () => {
		const { manager, sessions } = harness();
		const first = await manager.acquire(config(), vi.fn());
		const second = await manager.acquire(config('driveway'), vi.fn());
		expect(sessions[0]!.configure).toHaveBeenLastCalledWith([
			...config().sources,
			...config('driveway').sources
		]);
		await first.release();
		expect(sessions[0]!.configure).toHaveBeenLastCalledWith(config('driveway').sources);
		await second.release();
	});

	it('separates credentials and does not retain released listeners', async () => {
		const { manager, factory, notifications } = harness();
		const firstListener = vi.fn();
		const secondListener = vi.fn();
		const first = await manager.acquire(config(), firstListener);
		const second = await manager.acquire(config('front-door', 'other-credential'), secondListener);
		expect(factory).toHaveBeenCalledTimes(2);
		await first.release();
		firstListener.mockClear();
		notifications[0]!({ status: 'ready', message: null, cameras: [], streams: new Map() });
		expect(firstListener).not.toHaveBeenCalled();
		await second.release();
	});

	it('cancels an acquisition before allocating any session', async () => {
		const { manager, factory } = harness();
		const abort = new AbortController();
		const pending = manager.acquire(config(), vi.fn(), abort.signal);
		abort.abort();
		await expect(pending).rejects.toThrow(/removed/i);
		expect(factory).not.toHaveBeenCalled();
	});

	it('rejects capacity overflow without disturbing existing consumers', async () => {
		const { manager, sessions } = harness();
		const full = config();
		full.sources = Array.from({ length: 16 }, (_, index) => ({
			source_id: `source-${index}`,
			quality: 'auto'
		}));
		const lease = await manager.acquire(full, vi.fn());
		await expect(manager.acquire(config('overflow'), vi.fn())).rejects.toThrow(/16/);
		expect(sessions[0]!.configure).toHaveBeenCalledTimes(1);
		await lease.release();
	});

	it('waits for the previous session to close before reopening the same identity', async () => {
		const { manager, factory, sessions } = harness();
		let finishClose!: () => void;
		const first = await manager.acquire(config(), vi.fn());
		vi.mocked(sessions[0]!.close).mockImplementation(
			() => new Promise<void>((resolve) => (finishClose = resolve))
		);
		const closing = first.release();
		const reopening = manager.acquire(config(), vi.fn());
		await vi.waitFor(() => expect(sessions[0]!.close).toHaveBeenCalledTimes(1));
		expect(factory).toHaveBeenCalledTimes(1);
		finishClose();
		await closing;
		const second = await reopening;
		expect(factory).toHaveBeenCalledTimes(2);
		await second.release();
	});
});
