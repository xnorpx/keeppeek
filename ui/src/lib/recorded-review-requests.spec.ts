import { describe, expect, it } from 'vitest';
import { RecordedReviewRequests, reviewExportRecordings } from './recorded-review-requests';

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

it('queries only the UTC days touched by the half-open marks and stops after cancellation', async () => {
	const boundary = Date.UTC(2026, 8, 6);
	const dates: string[] = [];
	const client = {
		getRecordingsInRange: async (_cameras: readonly string[], date: string) => {
			dates.push(date);
			return [];
		}
	};
	const controller = new AbortController();
	await reviewExportRecordings(
		client,
		'front',
		{ startMs: boundary - 1, endMs: boundary },
		controller.signal
	);
	expect(dates).toEqual(['2026-09-05']);
	dates.length = 0;
	await reviewExportRecordings(
		client,
		'front',
		{ startMs: boundary - 1, endMs: boundary + 1 },
		controller.signal
	);
	expect(dates).toEqual(['2026-09-05', '2026-09-06']);
	controller.abort();
	await expect(
		reviewExportRecordings(
			client,
			'front',
			{ startMs: boundary - 1, endMs: boundary + 1 },
			controller.signal
		)
	).rejects.toThrow();
	expect(dates).toHaveLength(2);
});
