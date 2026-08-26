import { describe, expect, it, vi } from 'vitest';
import type {
	EventMetadataSearchOptions,
	StoredTimelineQueryOptions,
	StoredTimelineResult
} from './control-client';
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

	it('loads availability before a bounded event metadata page', async () => {
		const timelineCalls: StoredTimelineQueryOptions[] = [];
		const eventCalls: EventMetadataSearchOptions[] = [];
		const client = {
			queryStoredTimeline: vi.fn(async (options: StoredTimelineQueryOptions) => {
				timelineCalls.push(options);
				const result: StoredTimelineResult = {
					ranges: [{ sourceId: 'front-door', streamId: 'main', startMs: 100, endMs: 200 }],
					events: []
				};
				options.onPage?.(result);
				return result;
			}),
			searchEventMetadata: vi.fn(async (options: EventMetadataSearchOptions) => {
				eventCalls.push(options);
				return {
					hits: [
						{
							eventId: 'person-1',
							sourceId: 'front-door',
							eventType: 'person',
							origin: 'camera' as const,
							startMs: 150,
							endMs: null,
							confidence: 0.9,
							bbox: null,
							zone: null,
							text: null,
							hasImageAttachment: true,
							previewStartMs: 145,
							previewEndMs: 160,
							keyframes: [],
							keyframesTruncated: false
						}
					],
					nextPageToken: '',
					candidatesTruncated: false
				};
			})
		};
		const repository = new TimelineRepository(client);

		await repository.loadWindow({
			sourceIds: ['front-door'],
			startMs: 100,
			endMs: 200,
			bucketMs: 10,
			prefetchMs: 0
		});

		expect(timelineCalls[0]?.includeEvents).toBe(false);
		expect(eventCalls[0]).toMatchObject({
			sourceIds: ['front-door'],
			startMs: 100,
			endMs: 200,
			pageSize: 128
		});
		expect(repository.events[0]?.attachments).toHaveLength(1);
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

	it('clamps aligned prefetch to the selected recording day', async () => {
		const calls: Array<[number, number]> = [];
		const client = {
			queryStoredTimeline: vi.fn(async (options: StoredTimelineQueryOptions) => {
				calls.push([options.startMs, options.endMs]);
				return emptyResult;
			})
		};
		const repository = new TimelineRepository(client);

		await repository.loadWindow({
			sourceIds: ['front-door'],
			startMs: 100,
			endMs: 200,
			bucketMs: 50,
			prefetchMs: 1_000,
			minimumMs: 75,
			maximumMs: 225
		});

		expect(calls).toEqual([[75, 225]]);
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
						events: [{ ...event, revision: 1, thumbnail_url: 'blob:person-1' }]
					};
				}
				const result = { ranges: [], events: [event] };
				options.onPage?.(result);
				return result;
			})
		};
		const repository = new TimelineRepository(client, {
			get: async () => null,
			put: async () => undefined
		});

		await repository.loadWindow({
			sourceIds: ['front-door'],
			startMs: 100,
			endMs: 200,
			bucketMs: 10,
			prefetchMs: 0,
			viewportExtentPx: 600
		});
		await vi.waitFor(() => expect(repository.events[0]?.thumbnail_url).toBe('blob:person-1'));
		expect(repository.events).toHaveLength(1);
		expect(repository.events[0]?.revision).toBe(1);

		expect(calls).toHaveLength(2);
		expect(calls[0]?.includeAttachments).toBe(false);
		expect(calls[1]).toMatchObject({
			sourceIds: ['front-door'],
			startMs: 150,
			endMs: 151,
			includeAttachments: true
		});
	});

	it('does not re-touch a cached thumbnail for an unchanged viewport', async () => {
		const memoryHits: string[] = [];
		const handlePerformanceEvent = (event: Event) => {
			const detail = (event as CustomEvent<{ name: string; eventId?: string }>).detail;
			if (detail.name === 'ThumbnailCacheHitMemory' && detail.eventId) {
				memoryHits.push(detail.eventId);
			}
		};
		window.addEventListener('keeppeek:timeline-performance', handlePerformanceEvent);
		const event = {
			id: 'person-1',
			source_id: 'front-door',
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
				if (options.includeAttachments) {
					return {
						ranges: [],
						events: [{ ...event, revision: 1, thumbnail_url: 'blob:person-1' }]
					};
				}
				return { ranges: [], events: [event] };
			})
		};
		const repository = new TimelineRepository(client, {
			get: async () => null,
			put: async () => undefined
		});
		const request = {
			sourceIds: ['front-door'],
			startMs: 100,
			endMs: 200,
			bucketMs: 10,
			prefetchMs: 0,
			viewportExtentPx: 600
		};

		try {
			await repository.loadWindow(request);
			await vi.waitFor(() => expect(repository.events[0]?.thumbnail_url).toBe('blob:person-1'));
			await repository.loadWindow(request);
			memoryHits.length = 0;
			await repository.loadWindow(request);
			expect(memoryHits).toEqual([]);
		} finally {
			repository.dispose();
			window.removeEventListener('keeppeek:timeline-performance', handlePerformanceEvent);
		}
	});

	it('retains rendered data but reloads coverage after reconnect revalidation', async () => {
		let calls = 0;
		const client = {
			queryStoredTimeline: vi.fn(async (options: StoredTimelineQueryOptions) => {
				calls += 1;
				return {
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
			})
		};
		const repository = new TimelineRepository(client);
		const request = {
			sourceIds: ['front-door'],
			startMs: 100,
			endMs: 200,
			bucketMs: 10,
			prefetchMs: 0
		};

		await repository.loadWindow(request);
		expect(repository.ranges).toHaveLength(1);
		repository.revalidate();
		expect(repository.ranges).toHaveLength(1);
		await repository.loadWindow(request);

		expect(calls).toBe(2);
		expect(repository.ranges).toHaveLength(1);
	});
});
