import { keepModes, parseKeepMode, type KeepMode } from './keep-modes';
import type { RecordedQualityPreference } from './recorded-playback-policy';

export type RecordingMoment = {
	cameraId: string;
	streamPreference: RecordedQualityPreference;
	timestampMs: number;
	mode: KeepMode;
	eventId?: string | null;
	returnHref?: string | null;
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
};

const preferences = ['auto', 'high', 'low', 'main', 'sub'] as const;
const routeFields = ['camera', 'date', 'at', 'stream', 'mode', 'event', 'returnTo'];
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
	'event',
	'eventCamera'
];
const maximumTimestampMs = 253_402_300_799_999;

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
	const params = new URLSearchParams(search.length <= 8192 ? search : '');
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
	const invalid =
		search.length > 8192 ||
		params.size > 16 ||
		routeFields.some((field) => params.getAll(field).length > 1) ||
		(!cameraId && (params.has('at') || params.has('event'))) ||
		(params.has('camera') && !cameraId) ||
		(params.has('event') && eventId === null) ||
		(params.has('at') && timestampMs === null) ||
		(params.has('date') && date === null) ||
		(params.has('stream') && streamPreference === null) ||
		(params.has('mode') && !keepModes.includes(params.get('mode') as KeepMode)) ||
		(params.has('returnTo') && returnHref === null);
	return {
		cameraId,
		streamPreference,
		timestampMs,
		date,
		mode: parseKeepMode(params.get('mode')),
		eventId,
		returnHref,
		invalid
	};
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
	return new URLSearchParams({
		camera: moment.cameraId,
		date: new Date(moment.timestampMs).toISOString().slice(0, 10),
		at: String(moment.timestampMs),
		...(boundedText(moment.eventId ?? null) ? { event: moment.eventId! } : {}),
		stream: moment.streamPreference,
		...(moment.mode === 'timeline' ? {} : { mode: moment.mode }),
		...(returnHref ? { returnTo: returnHref } : {})
	});
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
