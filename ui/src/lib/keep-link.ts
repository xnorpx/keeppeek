import { maximumReviewCameras } from './grid-playback-session';
import { keepModes, MAX_EXPORT_DURATION_MS, parseKeepMode, type KeepMode } from './keep-modes';
import type { RecordedQualityPreference } from './recorded-playback-policy';

export type KeepReviewRange = { startMs: number; endMs: number };

export type KeepReviewSelection = {
	cameraIds: readonly string[];
	audioCameraId: string;
	range?: KeepReviewRange;
};

export type RecordingMoment = {
	cameraId: string;
	streamPreference: RecordedQualityPreference;
	timestampMs: number;
	mode: KeepMode;
	eventId?: string | null;
	returnHref?: string | null;
	review?: KeepReviewSelection;
};

export type KeepRoute = {
	cameraId: string;
	streamPreference: RecordedQualityPreference | null;
	timestampMs: number | null;
	date: string | null;
	mode: KeepMode;
	eventId: string | null;
	returnHref: string | null;
	invalid: boolean;
	review?: KeepReviewSelection;
};

const preferences = ['auto', 'high', 'low', 'main', 'sub'] as const;
const routeFields = [
	'camera',
	'date',
	'at',
	'stream',
	'mode',
	'event',
	'returnTo',
	'audio',
	'rangeStart',
	'rangeEnd'
];
const returnFields = [
	'date',
	'from',
	'to',
	'camera',
	'type',
	'source',
	'zone',
	'confidence',
	'image',
	'q',
	'review',
	'bookmarks',
	'event',
	'eventCamera'
];
const maximumTimestampMs = 253_402_300_799_999;
const maximumSearchLength = 8192;

function boundedText(value: string | null, maximumLength = 256): string | null {
	if (!value || !value.trim() || value.length > maximumLength) return null;
	if (
		[...value].some((character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127)
	)
		return null;
	return value;
}

function utcDate(value: string | null): string | null {
	if (!value || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return null;
	const timestamp = Date.parse(`${value}T00:00:00Z`);
	return Number.isFinite(timestamp) &&
		timestamp >= 0 &&
		new Date(timestamp).toISOString().slice(0, 10) === value
		? value
		: null;
}

export function parseMomentTimestamp(value: string | null): number | null {
	if (value === null || !/^\d{1,15}$/.test(value)) return null;
	const timestampMs = Number(value);
	return Number.isSafeInteger(timestampMs) && timestampMs >= 0 && timestampMs <= maximumTimestampMs
		? timestampMs
		: null;
}

export function safeEventReturnHref(
	value: string | null | undefined,
	eventsPath = '/events'
): string | null {
	if (
		!value ||
		!boundedText(value, 2048) ||
		!value.startsWith('/') ||
		value.startsWith('//') ||
		value.includes('\\')
	)
		return null;
	const origin = 'https://keeppeek.invalid';
	const parsed = new URL(value, origin);
	if (parsed.origin !== origin || parsed.pathname !== eventsPath || parsed.searchParams.size > 16)
		return null;
	const parameters = new URLSearchParams();
	for (const field of returnFields) {
		const content = boundedText(parsed.searchParams.get(field), 512);
		if (content && parsed.searchParams.getAll(field).length === 1) parameters.set(field, content);
	}
	const href = `${eventsPath}${parameters.size ? `?${parameters}` : ''}`;
	return href.length <= 2048 ? href : null;
}

export function parseKeepRoute(search: string, eventsPath = '/events'): KeepRoute {
	const params = new URLSearchParams(search.length <= maximumSearchLength ? search : '');
	const cameraId = boundedText(params.get('camera')) ?? '';
	const eventId = boundedText(params.get('event'));
	const timestampMs = parseMomentTimestamp(params.get('at'));
	const date =
		timestampMs === null
			? utcDate(params.get('date'))
			: new Date(timestampMs).toISOString().slice(0, 10);
	const requestedStream = params.get('stream');
	const streamPreference = preferences.includes(requestedStream as RecordedQualityPreference)
		? (requestedStream as RecordedQualityPreference)
		: null;
	const returnHref = safeEventReturnHref(params.get('returnTo'), eventsPath);
	const hasRange = params.has('rangeStart') || params.has('rangeEnd');
	const from = parseMomentTimestamp(params.get('rangeStart'));
	const to = parseMomentTimestamp(params.get('rangeEnd'));
	const range =
		from !== null && to !== null && from < to && to - from <= MAX_EXPORT_DURATION_MS
			? { startMs: from, endMs: to }
			: undefined;
	const review =
		timestampMs === null
			? null
			: reviewSelection(params.getAll('compare'), cameraId, params.get('audio') ?? cameraId, range);
	const invalid =
		search.length > maximumSearchLength ||
		params.size > 16 ||
		routeFields.some((field) => params.getAll(field).length > 1) ||
		(!cameraId && (params.has('at') || params.has('event'))) ||
		(params.has('camera') && !cameraId) ||
		(params.has('event') && eventId === null) ||
		(params.has('at') && timestampMs === null) ||
		(params.has('date') && date === null) ||
		(params.has('stream') && streamPreference === null) ||
		(params.has('mode') && !keepModes.includes(params.get('mode') as KeepMode)) ||
		(params.has('returnTo') && returnHref === null) ||
		((params.has('compare') || params.has('audio') || hasRange) && review === null) ||
		(hasRange && !range) ||
		(review !== null &&
			params.get('mode') === 'export' &&
			(!range || (streamPreference !== 'main' && streamPreference !== 'sub')));
	return {
		cameraId,
		streamPreference,
		timestampMs,
		date,
		mode: parseKeepMode(params.get('mode')),
		eventId,
		returnHref,
		invalid,
		...(review && !invalid ? { review } : {})
	};
}

function reviewSelection(
	cameraIds: readonly string[],
	focusId: string,
	audioCameraId: string,
	range?: KeepReviewRange
): KeepReviewSelection | null {
	if (cameraIds.length < 2 || cameraIds.length > maximumReviewCameras) return null;
	const unique = new Set(cameraIds);
	if (
		unique.size !== cameraIds.length ||
		!cameraIds.every((id) => boundedText(id)) ||
		!unique.has(focusId) ||
		!unique.has(audioCameraId)
	)
		return null;
	if (
		range &&
		(parseMomentTimestamp(String(range.startMs)) === null ||
			parseMomentTimestamp(String(range.endMs)) === null ||
			range.startMs >= range.endMs ||
			range.endMs - range.startMs > MAX_EXPORT_DURATION_MS)
	)
		return null;
	return { cameraIds: [...cameraIds], audioCameraId, ...(range ? { range } : {}) };
}

export function keepMomentSearchParams(
	moment: RecordingMoment,
	eventsPath = '/events'
): URLSearchParams {
	if (
		!boundedText(moment.cameraId) ||
		parseMomentTimestamp(String(moment.timestampMs)) === null ||
		!preferences.includes(moment.streamPreference) ||
		!keepModes.includes(moment.mode)
	) {
		throw new Error('Select a camera and a valid recording moment before copying a link.');
	}
	const returnHref = safeEventReturnHref(moment.returnHref, eventsPath);
	const params = new URLSearchParams({
		camera: moment.cameraId,
		date: new Date(moment.timestampMs).toISOString().slice(0, 10),
		at: String(moment.timestampMs),
		...(boundedText(moment.eventId ?? null) ? { event: moment.eventId! } : {}),
		stream: moment.streamPreference,
		...(moment.mode === 'timeline' ? {} : { mode: moment.mode }),
		...(returnHref ? { returnTo: returnHref } : {})
	});
	if (moment.review) {
		const selection = reviewSelection(
			moment.review.cameraIds,
			moment.cameraId,
			moment.review.audioCameraId,
			moment.review.range
		);
		if (!selection)
			throw new Error('Select two to four cameras and an audio source from that selection.');
		for (const id of selection.cameraIds) params.append('compare', id);
		if (selection.range) {
			params.set('rangeStart', String(selection.range.startMs));
			params.set('rangeEnd', String(selection.range.endMs));
		}
		if (selection.audioCameraId !== moment.cameraId) params.set('audio', selection.audioCameraId);
	}
	if (params.toString().length > maximumSearchLength - 1)
		throw new Error('This recording link is too long.');
	return params;
}

export function recordingMomentUrl(
	options: RecordingMoment & { origin: string; keepPath: string; eventsPath?: string }
): string {
	const origin = new URL(options.origin).origin;
	const url = new URL(options.keepPath, origin);
	if (
		!['http:', 'https:'].includes(url.protocol) ||
		url.origin !== origin ||
		url.search ||
		url.hash
	) {
		throw new Error('The recording link must use the local Keep route.');
	}
	url.search = keepMomentSearchParams(options, options.eventsPath).toString();
	return url.href;
}
