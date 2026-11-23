import { describe, expect, it } from 'vitest';
import { RecordedReviewRequests } from './recorded-review-requests';

describe('comparison recorder admission', () => {
	it('serializes operations across cameras and skips a withdrawn camera', async () => {
		const lane = new RecordedReviewRequests();
		const current = new AbortController();
		const removed = new AbortController();
		const first = Promise.withResolvers<void>();
		const order: number[] = [];
		const initial = lane.run(async () => {
			order.push(1);
			await first.promise;
		}, current.signal);
		const withdrawn = lane.run(async () => order.push(2), removed.signal);
		const final = lane.run(async () => order.push(3), current.signal);
		removed.abort();
		await Promise.resolve();
		expect(order).toEqual([1]);
		first.resolve();
		await initial;
		await expect(withdrawn).rejects.toThrow();
		await final;
		expect(order).toEqual([1, 3]);
	});
	it('releases a stalled pre-dispatch operation and prevents a late network send', async () => {
		const lane = new RecordedReviewRequests(),
			signal = new AbortController().signal;
		const connection = Promise.withResolvers<void>();
		const completed = Promise.withResolvers<boolean>();
		const pending = lane.run(async (beforeSend) => {
			await connection.promise;
			try {
				beforeSend();
				completed.resolve(true);
			} catch {
				completed.resolve(false);
			}
		}, signal);
		await expect(pending).rejects.toThrow('timed out');
		await expect(
			lane.run(async (beforeSend) => {
				beforeSend();
				return 'healthy';
			}, signal)
		).resolves.toBe('healthy');
		connection.resolve();
		await expect(completed.promise).resolves.toBe(false);
	}, 10_000);
});
