export type TimelinePerformanceEventName =
	| 'TimelineQueryStarted'
	| 'TimelineFirstPage'
	| 'TimelineQueryCompleted'
	| 'TimelineQueryCancelled'
	| 'ThumbnailCacheHitMemory'
	| 'ThumbnailCacheHitDisk'
	| 'ThumbnailFetched'
	| 'ScrubSeekQueued'
	| 'ScrubSeekSent'
	| 'ScrubPreviewRendered'
	| 'ReplayFirstFragment'
	| 'ReplayFirstFrame'
	| 'ReplayRefill'
	| 'GridTileAdmitted'
	| 'GridTileFrozen'
	| 'GridTileEvicted'
	| 'DecoderCapacity';

export type TimelinePerformanceEvent = {
	name: TimelinePerformanceEventName;
	atMs: number;
	sourceId?: string;
	queryId?: string;
	cursorId?: string;
	generation?: string;
	durationMs?: number;
	[key: string]: string | number | boolean | undefined;
};

export function emitTimelinePerformanceEvent(
	name: TimelinePerformanceEventName,
	detail: Omit<TimelinePerformanceEvent, 'name' | 'atMs'> = {}
): TimelinePerformanceEvent {
	const event: TimelinePerformanceEvent = {
		name,
		atMs: typeof performance === 'undefined' ? Date.now() : performance.now(),
		...detail
	};
	if (typeof window !== 'undefined') {
		window.dispatchEvent(new CustomEvent('keeppeek:timeline-performance', { detail: event }));
	}
	return event;
}
