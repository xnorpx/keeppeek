import { describe, expect, it, vi } from 'vitest';
import type { StoredTimelineQueryOptions, StoredTimelineResult } from './control-client';
import {
	mergeTimelineIntervals,
	subtractTimelineIntervals,
	TimelineRepository
} from './timeline-repository.svelte';

const emptyResult: StoredTimelineResult = { ranges: [], events: [] };

describe('TimelineRepository', () => {
	it('merges adjacent coverage and subtracts cached interiors', () => {
		expect(
			mergeTimelineIntervals([
				{ startMs: 20, endMs: 30 },
				{ startMs: 0, endMs: 10 },
				{ startMs: 10, endMs: 25 }
			])
		).toEqual([{ startMs: 0, endMs: 30 }]);
		expect(
			subtractTimelineIntervals(
				[{ startMs: 0, endMs: 100 }],
				[
					{ startMs: 10, endMs: 20 },
					{ startMs: 30, endMs: 70 }
				]
			)
		).toEqual([
			{ startMs: 0, endMs: 10 },
			{ startMs: 20, endMs: 30 },
			{ startMs: 70, endMs: 100 }
		]);
	});

	it('publishes pages immediately and queries only missing aligned windows', async () => {
		const calls: StoredTimelineQueryOptions[] = [];
		const client = {
			queryStoredTimeline: vi.fn(async (options: StoredTimelineQueryOptions) => {
				calls.push(options);
				const page: StoredTimelineResult = {
					ranges: [
						{
							sourceId: 'front-door',
							streamId: 'main',
							startMs: options.startMs,
							endMs: options.endMs
						}
					],
					events: []
				};
				options.onPage?.(page);
				return page;
			})
		};
		const repository = new TimelineRepository(client);

		await repository.loadWindow({
			sourceIds: ['front-door'],
			startMs: 110,
			endMs: 190,
			bucketMs: 50,
			prefetchMs: 10
		});
		expect(calls.map(({ startMs, endMs }) => [startMs, endMs])).toEqual([[100, 200]]);
		expect(repository.ranges).toHaveLength(1);

		await repository.loadWindow({
			sourceIds: ['front-door'],
			startMs: 150,
			endMs: 240,
			bucketMs: 50,
			prefetchMs: 10
		});
		expect(calls.map(({ startMs, endMs }) => [startMs, endMs])).toEqual([
			[100, 200],
			[200, 250]
		]);
	});

	it('uses prefetch hysteresis instead of querying each crossed bucket', async () => {
		const calls: Array<[number, number]> = [];
		const client = {
			queryStoredTimeline: vi.fn(async (options: StoredTimelineQueryOptions) => {
				calls.push([options.startMs, options.endMs]);
				return { ranges: [], events: [] };
			})
		};
		const repository = new TimelineRepository(client);
		const window = {
			sourceIds: ['front-door'],
			startMs: 1_000,
			endMs: 1_100,
			bucketMs: 10,
			prefetchMs: 100
		};

		await repository.loadWindow(window);
		await repository.loadWindow({ ...window, startMs: 1_010, endMs: 1_110 });
		expect(calls).toEqual([[900, 1_200]]);

		await repository.loadWindow({ ...window, startMs: 1_060, endMs: 1_160 });
		expect(calls).toEqual([
			[900, 1_200],
			[1_200, 1_260]
		]);
	});

	it('cancels the previous generation when the source changes', async () => {
		const signals: AbortSignal[] = [];
		const client = {
			queryStoredTimeline: (options: StoredTimelineQueryOptions) => {
				signals.push(options.signal!);
				return new Promise<StoredTimelineResult>((resolve, reject) => {
					options.signal?.addEventListener('abort', () =>
						reject(new DOMException('cancelled', 'AbortError'))
					);
					if (options.sourceIds.includes('garage')) resolve(emptyResult);
				});
			}
		};
		const repository = new TimelineRepository(client);
		const first = repository.loadWindow({
			sourceIds: ['front-door'],
			startMs: 0,
			endMs: 100,
			bucketMs: 10,
			prefetchMs: 0
		});

		await repository.loadWindow({
			sourceIds: ['garage'],
			startMs: 0,
			endMs: 100,
			bucketMs: 10,
			prefetchMs: 0
		});
		await first;

		expect(signals[0]?.aborted).toBe(true);
		expect(repository.loading).toBe(false);
	});

	it('loads attachment-free metadata before a sparse thumbnail query', async () => {
		const calls: StoredTimelineQueryOptions[] = [];
		const event = {
			id: 'person-1',
			source_id: 'front-door',
			revision: 1,
			source: 'camera' as const,
			kind: 'person',
			start_time_ms: 150,
			end_time_ms: null,
			confidence: 0.9,
			bbox: null,
			zone: null,
			thumbnail_url: null,
			attachments: [
				{
					id: 'thumb-1',
					type: 'thumbnail',
					content_type: 'image/jpeg',
					byte_length: 100,
					ordinal: 0,
					timestamp_ms: 150
				}
			]
		};
		const client = {
			queryStoredTimeline: vi.fn(async (options: StoredTimelineQueryOptions) => {
				calls.push(options);
				if (options.includeAttachments) {
					return {
						ranges: [],
						events: [{ ...event, thumbnail_url: 'blob:person-1' }]
					};
				}
				const result = { ranges: [], events: [event] };
				options.onPage?.(result);
				return result;
			})
		};
		const repository = new TimelineRepository(client);

		await repository.loadWindow({
			sourceIds: ['front-door'],
			startMs: 100,
			endMs: 200,
			bucketMs: 10,
			prefetchMs: 0,
			viewportExtentPx: 600
		});
		await vi.waitFor(() => expect(repository.events[0]?.thumbnail_url).toBe('blob:person-1'));

		expect(calls).toHaveLength(2);
		expect(calls[0]?.includeAttachments).toBe(false);
		expect(calls[1]).toMatchObject({
			sourceIds: ['front-door'],
			startMs: 150,
			endMs: 151,
			includeAttachments: true
		});
	});
});