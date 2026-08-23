import type {
	ControlClient,
	StoredTimelineQueryOptions,
	StoredTimelineRange,
	StoredTimelineResult
} from './control-client';
import type { RecordingEvent } from './types';
import { emitTimelinePerformanceEvent } from './timeline-observability';
import {
	TimelineThumbnailDiskCache,
	type TimelineThumbnailIdentity
} from './timeline-thumbnail-disk-cache';

const MAX_CACHE_CONTEXTS = 6;
const MAX_EVENTS = 10_000;
const MAX_THUMBNAIL_BYTES = 1_048_576;
const MAX_ACTIVE_THUMBNAILS = 2;
const MIN_THUMBNAIL_SPACING_PX = 96;
const MAX_DECODED_THUMBNAIL_BYTES = 24 * 1_048_576;
const DECODED_THUMBNAIL_ESTIMATE_BYTES = 320 * 320 * 4;

export type TimelineInterval = {
	startMs: number;
	endMs: number;
};

export type TimelineWindowRequest = {
	sourceIds: readonly string[];
	startMs: number;
	endMs: number;
	bucketMs: number;
	prefetchMs: number;
	eventTypes?: readonly string[];
	includeEvents?: boolean;
	viewportExtentPx?: number;
};

export type TimelineViewport = Omit<
	TimelineWindowRequest,
	'sourceIds' | 'includeEvents' | 'viewportExtentPx'
> & {
	viewportExtentPx: number;
};

type TimelineQueryClient = Pick<ControlClient, 'queryStoredTimeline'> &
	Partial<Pick<ControlClient, 'releaseObjectUrl'>>;

type TimelineCache = {
	key: string;
	ranges: StoredTimelineRange[];
	events: RecordingEvent[];
	coverage: TimelineInterval[];
	lastUsed: number;
};

type ActiveQuery = {
	contextKey: string;
	generation: number;
	interval: TimelineInterval;
	controller: AbortController;
};

type ThumbnailTask = {
	contextKey: string;
	generation: number;
	cache: TimelineCache;
	event: RecordingEvent;
	sourceId: string;
	key: string;
};

type CachedThumbnail = {
	identity: string;
	cache: TimelineCache;
	bytes: number;
};

export class TimelineRepository {
	ranges = $state.raw<StoredTimelineRange[]>([]);
	events = $state.raw<RecordingEvent[]>([]);
	loading = $state(false);
	error = $state<string | null>(null);

	#client: TimelineQueryClient;
	#caches = new Map<string, TimelineCache>();
	#activeQueries = new Map<string, ActiveQuery>();
	#activeContextKey = '';
	#generation = 0;
	#nextQueryId = 1;
	#thumbnailQueue: ThumbnailTask[] = [];
	#thumbnailControllers = new Map<AbortController, string>();
	#thumbnailRequested = new Set<string>();
	#activeThumbnails = 0;
	#thumbnailUrls = new Map<string, CachedThumbnail>();
	#thumbnailByIdentity = new Map<string, string>();
	#decodedThumbnailBytes = 0;
	#diskCache = new TimelineThumbnailDiskCache();

	constructor(client: TimelineQueryClient) {
		this.#client = client;
	}

	async loadWindow(request: TimelineWindowRequest): Promise<void> {
		validateWindow(request);
		const sourceIds = [...new Set(request.sourceIds)].toSorted();
		const eventTypes = [...new Set(request.eventTypes ?? [])].toSorted();
		const includeEvents = request.includeEvents ?? true;
		const contextKey = JSON.stringify({
			sourceIds,
			bucketMs: request.bucketMs,
			eventTypes,
			includeEvents
		});
		const cache = this.#activate(contextKey);
		const targetInterval = alignedPrefetchWindow(request, 1);
		const requiredInterval = alignedPrefetchWindow(request, 0.5);
		this.#cancelQueriesOutside(contextKey, targetInterval);
		const inFlight = [...this.#activeQueries.values()]
			.filter((query) => query.contextKey === contextKey && !query.controller.signal.aborted)
			.map((query) => query.interval);
		const coverage = [...cache.coverage, ...inFlight];
		const requiredMissing = subtractTimelineIntervals([requiredInterval], coverage);
		const missing =
			requiredMissing.length > 0 ? subtractTimelineIntervals([targetInterval], coverage) : [];
		if (missing.length > 0) {
			this.error = null;
			await Promise.all(
				missing.map((missingInterval) =>
					this.#loadInterval({
						contextKey,
						generation: this.#generation,
						cache,
						interval: missingInterval,
						sourceIds,
						eventTypes,
						includeEvents,
						bucketMs: request.bucketMs
					})
				)
			);
		}
		if (request.viewportExtentPx) {
			this.#queueThumbnails(contextKey, this.#generation, cache, sourceIds, {
				startMs: request.startMs,
				endMs: request.endMs,
				viewportExtentPx: request.viewportExtentPx
			});
		}
	}

	invalidate(startMs: number, endMs: number): void {
		if (endMs <= startMs) return;
		const cache = this.#caches.get(this.#activeContextKey);
		if (!cache) return;
		cache.coverage = subtractTimelineIntervals(cache.coverage, [{ startMs, endMs }]);
	}

	revalidate(): void {
		this.#generation += 1;
		for (const query of this.#activeQueries.values()) query.controller.abort();
		this.#cancelThumbnails();
		for (const cache of this.#caches.values()) cache.coverage = [];
		this.#refreshLoading();
	}

	deactivate(): void {
		this.#generation += 1;
		for (const query of this.#activeQueries.values()) query.controller.abort();
		this.#cancelThumbnails();
		this.#activeContextKey = '';
		this.ranges = [];
		this.events = [];
		this.error = null;
		this.#refreshLoading();
	}

	dispose(): void {
		this.#generation += 1;
		for (const query of this.#activeQueries.values()) query.controller.abort();
		this.#cancelThumbnails();
		this.#activeQueries.clear();
		this.#releaseAllThumbnails();
		this.loading = false;
	}

	#activate(contextKey: string): TimelineCache {
		let cache = this.#caches.get(contextKey);
		if (!cache) {
			cache = { key: contextKey, ranges: [], events: [], coverage: [], lastUsed: 0 };
			this.#caches.set(contextKey, cache);
		}
		cache.lastUsed = Date.now();
		if (contextKey === this.#activeContextKey) return cache;

		this.#generation += 1;
		for (const query of this.#activeQueries.values()) query.controller.abort();
		this.#cancelThumbnails();
		this.#activeContextKey = contextKey;
		this.ranges = cache.ranges;
		this.events = cache.events;
		this.error = null;
		this.#evictCaches();
		this.#refreshLoading();
		return cache;
	}

	async #loadInterval(options: {
		contextKey: string;
		generation: number;
		cache: TimelineCache;
		interval: TimelineInterval;
		sourceIds: string[];
		eventTypes: string[];
		includeEvents: boolean;
		bucketMs: number;
	}): Promise<void> {
		const queryKey = `${options.contextKey}:${options.interval.startMs}:${options.interval.endMs}:${this.#nextQueryId++}`;
		const controller = new AbortController();
		const startedAtMs = performance.now();
		let firstPage = true;
		this.#activeQueries.set(queryKey, {
			contextKey: options.contextKey,
			generation: options.generation,
			interval: options.interval,
			controller
		});
		this.#refreshLoading();
		emitTimelinePerformanceEvent('TimelineQueryStarted', {
			queryId: queryKey,
			sourceId: options.sourceIds.join(','),
			startMs: options.interval.startMs,
			endMs: options.interval.endMs,
			bucketMs: options.bucketMs
		});

		const queryOptions: StoredTimelineQueryOptions = {
			sourceIds: options.sourceIds,
			startMs: options.interval.startMs,
			endMs: options.interval.endMs,
			availabilityBucketMs: options.bucketMs,
			eventTypes: options.eventTypes,
			includeEvents: options.includeEvents,
			includeAttachments: false,
			signal: controller.signal,
			onPage: (page) => {
				if (!this.#isCurrent(options.contextKey, options.generation, controller)) return;
				if (firstPage) {
					firstPage = false;
					emitTimelinePerformanceEvent('TimelineFirstPage', {
						queryId: queryKey,
						sourceId: options.sourceIds.join(','),
						durationMs: performance.now() - startedAtMs
					});
				}
				this.#mergePage(options.cache, page);
			}
		};

		try {
			const result = await this.#client.queryStoredTimeline(queryOptions);
			if (!this.#isCurrent(options.contextKey, options.generation, controller)) return;
			this.#mergePage(options.cache, result);
			options.cache.coverage = mergeTimelineIntervals([
				...options.cache.coverage,
				options.interval
			]);
			emitTimelinePerformanceEvent('TimelineQueryCompleted', {
				queryId: queryKey,
				sourceId: options.sourceIds.join(','),
				durationMs: performance.now() - startedAtMs
			});
		} catch (cause) {
			if (controller.signal.aborted || isAbortError(cause)) {
				emitTimelinePerformanceEvent('TimelineQueryCancelled', {
					queryId: queryKey,
					sourceId: options.sourceIds.join(','),
					durationMs: performance.now() - startedAtMs
				});
				return;
			}
			if (this.#isCurrent(options.contextKey, options.generation, controller)) {
				this.error = cause instanceof Error ? cause.message : 'Unable to load timeline metadata.';
			}
			throw cause;
		} finally {
			this.#activeQueries.delete(queryKey);
			this.#refreshLoading();
		}
	}

	#mergePage(cache: TimelineCache, page: StoredTimelineResult): void {
		cache.ranges = mergeTimelineRanges([...cache.ranges, ...page.ranges]);
		cache.events = mergeTimelineEvents(cache.events, page.events);
		for (const event of page.events) {
			if (event.thumbnail_url) this.#rememberThumbnail(cache, event);
		}
		cache.lastUsed = Date.now();
		if (cache === this.#caches.get(this.#activeContextKey)) {
			this.ranges = cache.ranges;
			this.events = cache.events;
		}
	}

	#cancelQueriesOutside(contextKey: string, interval: TimelineInterval): void {
		for (const query of this.#activeQueries.values()) {
			if (query.contextKey !== contextKey || intervalsOverlap(query.interval, interval)) continue;
			query.controller.abort();
		}
	}

	#queueThumbnails(
		contextKey: string,
		generation: number,
		cache: TimelineCache,
		sourceIds: readonly string[],
		window: { startMs: number; endMs: number; viewportExtentPx: number }
	): void {
		const durationMs = window.endMs - window.startMs;
		if (durationMs <= 0 || window.viewportExtentPx <= 0) return;
		const candidateStartMs = window.startMs - durationMs / 2;
		const candidateEndMs = window.endMs + durationMs / 2;
		const spacingMs = (durationMs * MIN_THUMBNAIL_SPACING_PX) / window.viewportExtentPx;
		const sourceSet = new Set(sourceIds);
		const candidates = cache.events
			.filter((event) => {
				const sourceId = event.source_id ?? sourceIds[0];
				const thumbnail = event.attachments?.find((attachment) => attachment.type === 'thumbnail');
				return (
					!!sourceId &&
					sourceSet.has(sourceId) &&
					event.start_time_ms >= candidateStartMs &&
					event.start_time_ms <= candidateEndMs &&
					!event.thumbnail_url &&
					!!thumbnail &&
					(thumbnail.byte_length === null || thumbnail.byte_length <= MAX_THUMBNAIL_BYTES)
				);
			})
			.toSorted(compareThumbnailPriority);
		for (const event of cache.events) {
			if (
				event.thumbnail_url &&
				event.start_time_ms >= candidateStartMs &&
				event.start_time_ms <= candidateEndMs
			) {
				this.#touchThumbnail(event.thumbnail_url);
				emitTimelinePerformanceEvent('ThumbnailCacheHitMemory', {
					sourceId: event.source_id ?? event.source,
					eventId: event.id,
					revision: event.revision ?? 0
				});
			}
		}
		const selected: RecordingEvent[] = [];
		for (const event of candidates) {
			if (
				selected.some(
					(candidate) => Math.abs(candidate.start_time_ms - event.start_time_ms) < spacingMs
				)
			) {
				continue;
			}
			selected.push(event);
		}

		for (const event of selected) {
			const sourceId = event.source_id ?? sourceIds[0];
			if (!sourceId) continue;
			const key = `${contextKey}:${sourceId}:${event.id}:${event.revision ?? 0}`;
			if (this.#thumbnailRequested.has(key)) continue;
			this.#thumbnailRequested.add(key);
			this.#thumbnailQueue.push({ contextKey, generation, cache, event, sourceId, key });
		}
		this.#drainThumbnailQueue();
	}

	#drainThumbnailQueue(): void {
		while (this.#activeThumbnails < MAX_ACTIVE_THUMBNAILS) {
			const task = this.#thumbnailQueue.shift();
			if (!task) return;
			if (task.contextKey !== this.#activeContextKey || task.generation !== this.#generation) {
				continue;
			}
			this.#activeThumbnails += 1;
			void this.#fetchThumbnail(task).finally(() => {
				this.#activeThumbnails -= 1;
				this.#drainThumbnailQueue();
			});
		}
	}

	async #fetchThumbnail(task: ThumbnailTask): Promise<void> {
		const controller = new AbortController();
		this.#thumbnailControllers.set(controller, task.key);
		try {
			const identity = thumbnailDiskIdentity(task.event, task.sourceId);
			const cachedBlob = identity ? await this.#diskCache.get(identity).catch(() => null) : null;
			if (cachedBlob) {
				if (!this.#isCurrent(task.contextKey, task.generation, controller)) return;
				this.#mergePage(task.cache, {
					ranges: [],
					events: [
						{
							...task.event,
							thumbnail_url: URL.createObjectURL(cachedBlob),
							thumbnail_blob: cachedBlob
						}
					]
				});
				emitTimelinePerformanceEvent('ThumbnailCacheHitDisk', {
					sourceId: task.sourceId,
					eventId: task.event.id,
					revision: task.event.revision ?? 0
				});
				return;
			}
			const result = await this.#client.queryStoredTimeline({
				sourceIds: [task.sourceId],
				startMs: task.event.start_time_ms,
				endMs: Math.max(
					task.event.start_time_ms + 1,
					(task.event.end_time_ms ?? task.event.start_time_ms) + 1
				),
				eventTypes: [task.event.kind],
				includeEvents: true,
				includeAttachments: true,
				signal: controller.signal
			});
			if (!this.#isCurrent(task.contextKey, task.generation, controller)) return;
			this.#mergePage(task.cache, { ranges: [], events: result.events });
			emitTimelinePerformanceEvent('ThumbnailFetched', {
				sourceId: task.sourceId,
				eventId: task.event.id,
				revision: task.event.revision ?? 0
			});
		} catch (cause) {
			if (controller.signal.aborted || isAbortError(cause))
				this.#thumbnailRequested.delete(task.key);
		} finally {
			this.#thumbnailControllers.delete(controller);
		}
	}

	#cancelThumbnails(): void {
		for (const task of this.#thumbnailQueue) this.#thumbnailRequested.delete(task.key);
		this.#thumbnailQueue = [];
		for (const [controller, key] of this.#thumbnailControllers) {
			this.#thumbnailRequested.delete(key);
			controller.abort();
		}
	}

	#rememberThumbnail(cache: TimelineCache, event: RecordingEvent): void {
		const url = event.thumbnail_url;
		if (!url) return;
		const identity = `${cache.key}:${event.source_id ?? event.source}:${event.id}:${event.revision ?? 0}`;
		const previousUrl = this.#thumbnailByIdentity.get(identity);
		if (previousUrl === url) {
			this.#touchThumbnail(url);
			return;
		}
		if (previousUrl) this.#releaseThumbnail(previousUrl, false);
		const descriptorBytes = event.attachments?.find(
			(attachment) => attachment.type === 'thumbnail'
		)?.byte_length;
		const bytes = Math.max(DECODED_THUMBNAIL_ESTIMATE_BYTES, descriptorBytes ?? 0);
		this.#thumbnailUrls.set(url, { identity, cache, bytes });
		this.#thumbnailByIdentity.set(identity, url);
		this.#decodedThumbnailBytes += bytes;
		const diskIdentity = thumbnailDiskIdentity(event, event.source_id ?? event.source);
		if (diskIdentity && event.thumbnail_blob) {
			void this.#diskCache.put(diskIdentity, event.thumbnail_blob).catch(() => undefined);
		}
		while (this.#decodedThumbnailBytes > MAX_DECODED_THUMBNAIL_BYTES) {
			const oldestUrl = this.#thumbnailUrls.keys().next().value as string | undefined;
			if (!oldestUrl) break;
			this.#releaseThumbnail(oldestUrl, true);
		}
	}

	#touchThumbnail(url: string): void {
		const thumbnail = this.#thumbnailUrls.get(url);
		if (!thumbnail) return;
		this.#thumbnailUrls.delete(url);
		this.#thumbnailUrls.set(url, thumbnail);
	}

	#releaseThumbnail(url: string, clearEvent: boolean): void {
		const thumbnail = this.#thumbnailUrls.get(url);
		if (!thumbnail) return;
		this.#thumbnailUrls.delete(url);
		this.#thumbnailByIdentity.delete(thumbnail.identity);
		this.#decodedThumbnailBytes = Math.max(0, this.#decodedThumbnailBytes - thumbnail.bytes);
		this.#client.releaseObjectUrl?.(url);
		if (!clearEvent) return;
		thumbnail.cache.events = thumbnail.cache.events.map((event) =>
			event.thumbnail_url === url ? { ...event, thumbnail_url: null } : event
		);
		this.#thumbnailRequested.delete(thumbnail.identity);
		if (thumbnail.cache === this.#caches.get(this.#activeContextKey)) {
			this.events = thumbnail.cache.events;
		}
	}

	#releaseAllThumbnails(): void {
		for (const url of this.#thumbnailUrls.keys()) this.#releaseThumbnail(url, false);
	}

	#isCurrent(contextKey: string, generation: number, controller: AbortController): boolean {
		return (
			!controller.signal.aborted &&
			this.#activeContextKey === contextKey &&
			this.#generation === generation
		);
	}

	#refreshLoading(): void {
		this.loading = [...this.#activeQueries.values()].some(
			(query) => query.contextKey === this.#activeContextKey && !query.controller.signal.aborted
		);
	}

	#evictCaches(): void {
		if (this.#caches.size <= MAX_CACHE_CONTEXTS) return;
		const oldest = [...this.#caches.entries()]
			.filter(([key]) => key !== this.#activeContextKey)
			.toSorted((left, right) => left[1].lastUsed - right[1].lastUsed)[0];
		if (oldest) this.#caches.delete(oldest[0]);
	}
}

export function mergeTimelineIntervals(intervals: readonly TimelineInterval[]): TimelineInterval[] {
	const ordered = intervals
		.filter((interval) => interval.endMs > interval.startMs)
		.map((interval) => ({ ...interval }))
		.toSorted((left, right) => left.startMs - right.startMs || left.endMs - right.endMs);
	const merged: TimelineInterval[] = [];
	for (const interval of ordered) {
		const previous = merged.at(-1);
		if (previous && interval.startMs <= previous.endMs) {
			previous.endMs = Math.max(previous.endMs, interval.endMs);
		} else {
			merged.push(interval);
		}
	}
	return merged;
}

export function subtractTimelineIntervals(
	requested: readonly TimelineInterval[],
	covered: readonly TimelineInterval[]
): TimelineInterval[] {
	const coverage = mergeTimelineIntervals(covered);
	const missing: TimelineInterval[] = [];
	for (const interval of mergeTimelineIntervals(requested)) {
		let cursor = interval.startMs;
		for (const cached of coverage) {
			if (cached.endMs <= cursor) continue;
			if (cached.startMs >= interval.endMs) break;
			if (cached.startMs > cursor) {
				missing.push({ startMs: cursor, endMs: Math.min(cached.startMs, interval.endMs) });
			}
			cursor = Math.max(cursor, cached.endMs);
			if (cursor >= interval.endMs) break;
		}
		if (cursor < interval.endMs) missing.push({ startMs: cursor, endMs: interval.endMs });
	}
	return missing;
}

function alignedPrefetchWindow(
	request: TimelineWindowRequest,
	prefetchFactor: number
): TimelineInterval {
	const prefetchMs = request.prefetchMs * prefetchFactor;
	const unalignedStart = Math.max(0, request.startMs - prefetchMs);
	const unalignedEnd = request.endMs + prefetchMs;
	return {
		startMs: Math.floor(unalignedStart / request.bucketMs) * request.bucketMs,
		endMs: Math.ceil(unalignedEnd / request.bucketMs) * request.bucketMs
	};
}

function mergeTimelineRanges(ranges: readonly StoredTimelineRange[]): StoredTimelineRange[] {
	const ordered = ranges
		.filter((range) => range.endMs > range.startMs)
		.map((range) => ({ ...range }))
		.toSorted(
			(left, right) =>
				left.sourceId.localeCompare(right.sourceId) ||
				left.streamId.localeCompare(right.streamId) ||
				left.startMs - right.startMs ||
				left.endMs - right.endMs
		);
	const merged: StoredTimelineRange[] = [];
	for (const range of ordered) {
		const previous = merged.at(-1);
		if (
			previous &&
			previous.sourceId === range.sourceId &&
			previous.streamId === range.streamId &&
			range.startMs <= previous.endMs
		) {
			previous.endMs = Math.max(previous.endMs, range.endMs);
		} else {
			merged.push(range);
		}
	}
	return merged;
}

function mergeTimelineEvents(
	current: readonly RecordingEvent[],
	incoming: readonly RecordingEvent[]
): RecordingEvent[] {
	const byKey = new Map<string, RecordingEvent>();
	for (const event of [...current, ...incoming]) {
		const key = `${event.source_id ?? event.source}:${event.id}:${event.revision ?? 0}`;
		const previous = byKey.get(key);
		if (!previous || (!previous.thumbnail_url && event.thumbnail_url)) byKey.set(key, event);
	}
	return [...byKey.values()]
		.toSorted((left, right) => right.start_time_ms - left.start_time_ms)
		.slice(0, MAX_EVENTS);
}

function compareThumbnailPriority(left: RecordingEvent, right: RecordingEvent): number {
	return (
		thumbnailKindPriority(right.kind) - thumbnailKindPriority(left.kind) ||
		(right.confidence ?? -1) - (left.confidence ?? -1) ||
		right.start_time_ms - left.start_time_ms ||
		left.id.localeCompare(right.id)
	);
}

function thumbnailDiskIdentity(
	event: RecordingEvent,
	sourceId: string
): TimelineThumbnailIdentity | null {
	const attachment = event.attachments?.find((candidate) => candidate.type === 'thumbnail');
	if (!attachment) return null;
	return {
		sourceId,
		eventId: event.id,
		revision: event.revision ?? 0,
		attachmentId: attachment.id,
		sizeClass: 320
	};
}

function thumbnailKindPriority(kind: string): number {
	const words = kind.toLocaleLowerCase().replaceAll(/[-_]/g, ' ').split(/\s+/);
	if (words.includes('doorbell') || words.includes('entry')) return 4;
	if (words.includes('person') || words.includes('face')) return 3;
	if (words.includes('vehicle') || words.includes('object')) return 2;
	if (words.includes('motion')) return 1;
	return 0;
}

function validateWindow(request: TimelineWindowRequest): void {
	if (
		request.sourceIds.length === 0 ||
		!Number.isFinite(request.startMs) ||
		!Number.isFinite(request.endMs) ||
		request.endMs <= request.startMs ||
		!Number.isFinite(request.bucketMs) ||
		request.bucketMs <= 0 ||
		!Number.isFinite(request.prefetchMs) ||
		request.prefetchMs < 0
	) {
		throw new Error('Timeline window is invalid.');
	}
}

function intervalsOverlap(left: TimelineInterval, right: TimelineInterval): boolean {
	return left.startMs < right.endMs && left.endMs > right.startMs;
}

function isAbortError(cause: unknown): boolean {
	return cause instanceof DOMException && cause.name === 'AbortError';
}
