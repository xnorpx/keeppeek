import type { CameraListItem, RecordingEvent } from './types';

export type EventImageFilter = 'all' | 'with' | 'without';

export type EventPreviewState = 'idle' | 'queued' | 'loading' | 'unavailable';

export type EventBrowserFilters = {
	date: string;
	cameraId: string | null;
	type: string | null;
	source: RecordingEvent['source'] | null;
	minimumConfidence: number | null;
	image: EventImageFilter;
	query: string;
};

export type EventBrowserRecord = {
	camera: CameraListItem;
	event: RecordingEvent;
};

export type EventNoResultsSuggestion = {
	label: string;
	count: number;
	update: Partial<EventBrowserFilters>;
};

const ISO_DATE = /^\d{4}-\d{2}-\d{2}$/;
export const EVENT_BROWSER_PAGE_SIZE = 18;
export const EVENT_BROWSER_INITIAL_WINDOW_MS = 5 * 60_000;
const EVENT_BROWSER_MAX_WINDOW_MS = 6 * 60 * 60_000;

export type EventBrowserTimeWindow = {
	startMs: number;
	endMs: number;
	nextDurationMs: number;
};

export function eventBrowserDayBounds(
	date: string,
	nowMs = Date.now()
): { startMs: number; endMs: number } {
	const startMs = Date.parse(`${date}T00:00:00Z`);
	if (!Number.isFinite(startMs) || new Date(startMs).toISOString().slice(0, 10) !== date) {
		throw new Error('Event date is invalid.');
	}
	const dayEndMs = startMs + 86_400_000;
	return { startMs, endMs: nowMs >= startMs && nowMs < dayEndMs ? nowMs : dayEndMs };
}

export function previousEventBrowserWindow(
	dayStartMs: number,
	cursorMs: number,
	durationMs: number
): EventBrowserTimeWindow | null {
	if (cursorMs <= dayStartMs) return null;
	return {
		startMs: Math.max(dayStartMs, cursorMs - durationMs),
		endMs: cursorMs,
		nextDurationMs: Math.min(durationMs * 2, EVENT_BROWSER_MAX_WINDOW_MS)
	};
}

export function parseEventBrowserFilters(
	params: URLSearchParams,
	fallbackDate: string
): EventBrowserFilters {
	const requestedDate = params.get('date');
	const confidenceValue = params.get('confidence');
	const parsedConfidence = confidenceValue === null ? Number.NaN : Number(confidenceValue);
	const requestedSource = params.get('source');
	const requestedImage = params.get('image');
	return {
		date: requestedDate !== null && ISO_DATE.test(requestedDate) ? requestedDate : fallbackDate,
		cameraId: clean(params.get('camera')),
		type: clean(params.get('type')),
		source: requestedSource === 'camera' || requestedSource === 'keeppeek' ? requestedSource : null,
		minimumConfidence:
			Number.isFinite(parsedConfidence) && parsedConfidence >= 0 && parsedConfidence <= 1
				? parsedConfidence
				: null,
		image: requestedImage === 'with' || requestedImage === 'without' ? requestedImage : 'all',
		query: params.get('q')?.trim() ?? ''
	};
}

export function eventBrowserSearchParams(
	filters: EventBrowserFilters,
	selected: EventBrowserRecord | null = null
): URLSearchParams {
	return new URLSearchParams({
		date: filters.date,
		...(filters.cameraId ? { camera: filters.cameraId } : {}),
		...(filters.type ? { type: filters.type } : {}),
		...(filters.source ? { source: filters.source } : {}),
		...(filters.minimumConfidence === null
			? {}
			: { confidence: String(filters.minimumConfidence) }),
		...(filters.image === 'all' ? {} : { image: filters.image }),
		...(filters.query ? { q: filters.query } : {}),
		...(selected ? { event: selected.event.id, eventCamera: selected.camera.id } : {})
	});
}

export function filterEventBrowserRecords(
	records: readonly EventBrowserRecord[],
	filters: EventBrowserFilters
): EventBrowserRecord[] {
	const query = filters.query.toLocaleLowerCase();
	return records
		.filter((record) => {
			if (filters.cameraId !== null && record.camera.id !== filters.cameraId) return false;
			if (
				filters.type !== null &&
				record.event.kind.toLocaleLowerCase() !== filters.type.toLocaleLowerCase()
			) {
				return false;
			}
			if (filters.source !== null && record.event.source !== filters.source) return false;
			if (
				filters.minimumConfidence !== null &&
				(record.event.confidence === null || record.event.confidence < filters.minimumConfidence)
			) {
				return false;
			}
			if (filters.image === 'with' && !eventHasImage(record.event)) return false;
			if (filters.image === 'without' && eventHasImage(record.event)) return false;
			if (!query) return true;
			return [
				record.event.kind,
				record.event.source,
				record.event.zone,
				record.camera.id,
				record.camera.name
			]
				.filter((value): value is string => value !== null)
				.some((value) => value.toLocaleLowerCase().includes(query));
		})
		.toSorted((left, right) => right.event.start_time_ms - left.event.start_time_ms);
}

export function eventHasImage(event: RecordingEvent): boolean {
	return (
		event.thumbnail_url !== null ||
		event.attachments?.some((attachment) => attachment.type === 'thumbnail') === true
	);
}

export function eventBrowserRecordKey(record: EventBrowserRecord): string {
	return `${encodeURIComponent(record.camera.id)}:${encodeURIComponent(record.event.id)}`;
}

export function eventFilterSummary(filters: EventBrowserFilters): string {
	const clauses = [
		filters.cameraId ? `camera ${filters.cameraId}` : null,
		filters.type ? `type ${filters.type}` : null,
		filters.source ? `source ${filters.source}` : null,
		filters.minimumConfidence === null ? null : `confidence at least ${filters.minimumConfidence}`,
		filters.image === 'with'
			? 'with images'
			: filters.image === 'without'
				? 'without images'
				: null,
		filters.query ? `matching “${filters.query}”` : null
	].filter((clause): clause is string => clause !== null);
	return clauses.length === 0 ? `on ${filters.date}` : `${clauses.join(', ')} on ${filters.date}`;
}

export function eventNoResultsSuggestion(
	records: readonly EventBrowserRecord[],
	filters: EventBrowserFilters
): EventNoResultsSuggestion | null {
	const candidates: Array<{
		label: (count: number) => string;
		update: Partial<EventBrowserFilters>;
	}> = [
		...(filters.query
			? [
					{
						label: (count: number) => `Clear “${filters.query}” · ${count} results`,
						update: { query: '' }
					}
				]
			: []),
		...(filters.minimumConfidence !== null
			? [
					{
						label: (count: number) => `Remove confidence limit · ${count} results`,
						update: { minimumConfidence: null }
					}
				]
			: []),
		...(filters.cameraId
			? [{ label: (count: number) => `Any camera · ${count} results`, update: { cameraId: null } }]
			: []),
		...(filters.type
			? [{ label: (count: number) => `Any event type · ${count} results`, update: { type: null } }]
			: []),
		...(filters.source
			? [{ label: (count: number) => `Any source · ${count} results`, update: { source: null } }]
			: []),
		...(filters.image !== 'all'
			? [
					{
						label: (count: number) => `Any image state · ${count} results`,
						update: { image: 'all' as const }
					}
				]
			: [])
	];
	return (
		candidates
			.map((candidate) => {
				const count = filterEventBrowserRecords(records, {
					...filters,
					...candidate.update
				}).length;
				return { label: candidate.label(count), count, update: candidate.update };
			})
			.filter((candidate) => candidate.count > 0)
			.toSorted((left, right) => left.count - right.count)[0] ?? null
	);
}

function clean(value: string | null): string | null {
	const cleaned = value?.trim() ?? '';
	return cleaned ? cleaned : null;
}
