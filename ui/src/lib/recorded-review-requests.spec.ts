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
});
