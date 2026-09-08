import { describe, expect, it, vi } from 'vitest';
import { PeekWakeLock, type PeekWakeLockState, type ScreenWakeLockHandle } from './peek-wake-lock';

class Sentinel extends EventTarget implements ScreenWakeLockHandle {
	released = false;
	async release(): Promise<void> {
		this.released = true;
		this.dispatchEvent(new Event('release'));
	}
}

function harness(request = vi.fn(async (): Promise<ScreenWakeLockHandle> => new Sentinel())) {
	const states: PeekWakeLockState[] = [];
	const wakeLock = new PeekWakeLock(request, (state) => states.push(state));
	return { wakeLock, request, states };
}

describe('PeekWakeLock', () => {
	it('does not request a saved preference before a user gesture', async () => {
		const { wakeLock, request, states } = harness();
		await wakeLock.setEnabled(true);
		expect(request).not.toHaveBeenCalled();
		expect(states.at(-1)).toBe('awaiting-gesture');
		await wakeLock.activate();
		expect(request).toHaveBeenCalledTimes(1);
		expect(states.at(-1)).toBe('active');
		await wakeLock.dispose();
	});

	it('releases on visibility loss and reacquires only when visible and enabled', async () => {
		const first = new Sentinel();
		const request = vi.fn(async () => first);
		const { wakeLock, states } = harness(request);
		await wakeLock.setEnabled(true, true);
		await wakeLock.setVisible(false);
		expect(first.released).toBe(true);
		expect(states.at(-1)).toBe('released');
		request.mockImplementation(async () => new Sentinel());
		await wakeLock.setVisible(true);
		expect(request).toHaveBeenCalledTimes(2);
		expect(states.at(-1)).toBe('active');
		await wakeLock.setEnabled(false);
		await wakeLock.setVisible(false);
		await wakeLock.setVisible(true);
		expect(request).toHaveBeenCalledTimes(2);
		expect(states.at(-1)).toBe('off');
		await wakeLock.dispose();
	});

	it('reports unsupported browsers without attempting a request', async () => {
		const states: PeekWakeLockState[] = [];
		const wakeLock = new PeekWakeLock(null, (state) => states.push(state));
		await wakeLock.setEnabled(true, true);
		expect(states.at(-1)).toBe('unsupported');
		await wakeLock.dispose();
	});

	it('reports denial without repeating requests on visibility or ordinary gestures', async () => {
		const request = vi.fn(async (): Promise<ScreenWakeLockHandle> => {
			throw new Error('NotAllowedError');
		});
		const { wakeLock, states } = harness(request);
		await wakeLock.setEnabled(true, true);
		expect(states.at(-1)).toBe('denied');
		await wakeLock.setVisible(false);
		await wakeLock.setVisible(true);
		await wakeLock.activate();
		expect(request).toHaveBeenCalledTimes(1);
		await wakeLock.dispose();
	});

	it.each(['hidden', 'off', 'disposed'] as const)(
		'releases a pending request that resolves after becoming %s',
		async (transition) => {
			const pending = Promise.withResolvers<ScreenWakeLockHandle>();
			const sentinel = new Sentinel();
			const { wakeLock, request, states } = harness(vi.fn(() => pending.promise));
			const acquiring = wakeLock.setEnabled(true, true);
			await wakeLock.activate();
			expect(request).toHaveBeenCalledTimes(1);
			if (transition === 'hidden') await wakeLock.setVisible(false);
			if (transition === 'off') await wakeLock.setEnabled(false);
			if (transition === 'disposed') await wakeLock.dispose();
			pending.resolve(sentinel);
			await acquiring;
			expect(sentinel.released).toBe(true);
			expect(states.at(-1)).not.toBe('active');
			await wakeLock.dispose();
		}
	);

	it('reports system release without immediately requesting another lock', async () => {
		const sentinel = new Sentinel();
		const { wakeLock, request, states } = harness(vi.fn(async () => sentinel));
		await wakeLock.setEnabled(true, true);
		await sentinel.release();
		expect(states.at(-1)).toBe('released');
		expect(request).toHaveBeenCalledTimes(1);
		await wakeLock.dispose();
	});

	it('releases the active lock on teardown and ignores later activation', async () => {
		const sentinel = new Sentinel();
		const { wakeLock, request } = harness(vi.fn(async () => sentinel));
		await wakeLock.setEnabled(true, true);
		await wakeLock.dispose();
		await wakeLock.activate();
		expect(sentinel.released).toBe(true);
		expect(request).toHaveBeenCalledTimes(1);
	});

	it.each(['toggle', 'visibility'] as const)(
		'reconciles a rapid %s change after an in-flight release finishes',
		async (change) => {
			const releasing = Promise.withResolvers<void>();
			const sentinel = new Sentinel();
			const release = sentinel.release.bind(sentinel);
			sentinel.release = async () => {
				await releasing.promise;
				await release();
			};
			const { wakeLock, request, states } = harness();
			request.mockResolvedValueOnce(sentinel);
			await wakeLock.setEnabled(true, true);
			const suspended =
				change === 'toggle' ? wakeLock.setEnabled(false) : wakeLock.setVisible(false);
			const resumed =
				change === 'toggle' ? wakeLock.setEnabled(true, true) : wakeLock.setVisible(true);
			expect(request).toHaveBeenCalledTimes(1);
			releasing.resolve();
			await Promise.all([suspended, resumed]);
			expect(request).toHaveBeenCalledTimes(2);
			expect(states.at(-1)).toBe('active');
			await wakeLock.dispose();
		}
	);
});
