import type { RecordedQualityPreference } from './recorded-playback-policy';
import type { LiveQuality } from './types';

export const playbackPreferencesStorageKey = 'keeppeek-playback-preferences';
export const playbackPreferencesVersion = 1 as const;

export type FocusedLivePreference = LiveQuality | 'main' | 'sub';

export type PlaybackPreferenceDocument = {
	version: typeof playbackPreferencesVersion;
	recorded: {
		default: RecordedQualityPreference;
		cameras: Record<string, RecordedQualityPreference>;
	};
	focusedLive: {
		default: FocusedLivePreference;
		cameras: Record<string, FocusedLivePreference>;
	};
	media: {
		muted: boolean;
		playbackRate: number;
		playing: boolean;
	};
};

export type PlaybackPreferenceStorage = Pick<Storage, 'getItem' | 'setItem'>;

const recordedPreferences = new Set<RecordedQualityPreference>([
	'auto',
	'high',
	'low',
	'main',
	'sub'
]);
const focusedLivePreferences = new Set<FocusedLivePreference>([
	'auto',
	'high',
	'low',
	'main',
	'sub'
]);

export function defaultPlaybackPreferences(): PlaybackPreferenceDocument {
	return {
		version: playbackPreferencesVersion,
		recorded: { default: 'auto', cameras: {} },
		focusedLive: { default: 'auto', cameras: {} },
		media: { muted: false, playbackRate: 1, playing: true }
	};
}

export function loadPlaybackPreferences(
	storage: PlaybackPreferenceStorage
): PlaybackPreferenceDocument {
	try {
		const value = storage.getItem(playbackPreferencesStorageKey);
		if (!value) return defaultPlaybackPreferences();
		return parsePlaybackPreferences(JSON.parse(value));
	} catch {
		return defaultPlaybackPreferences();
	}
}

export function savePlaybackPreferences(
	storage: PlaybackPreferenceStorage,
	document: PlaybackPreferenceDocument
): void {
	try {
		storage.setItem(playbackPreferencesStorageKey, JSON.stringify(document));
	} catch {
		return;
	}
}

export function recordedPreference(
	document: PlaybackPreferenceDocument,
	cameraId: string
): RecordedQualityPreference {
	return document.recorded.cameras[cameraId] ?? document.recorded.default;
}

export function focusedLivePreference(
	document: PlaybackPreferenceDocument,
	cameraId: string
): FocusedLivePreference {
	return document.focusedLive.cameras[cameraId] ?? document.focusedLive.default;
}

export function withRecordedPreference(
	document: PlaybackPreferenceDocument,
	cameraId: string,
	preference: RecordedQualityPreference
): PlaybackPreferenceDocument {
	return {
		...document,
		recorded: {
			...document.recorded,
			cameras: { ...document.recorded.cameras, [cameraId]: preference }
		}
	};
}

export function withFocusedLivePreference(
	document: PlaybackPreferenceDocument,
	cameraId: string,
	preference: FocusedLivePreference
): PlaybackPreferenceDocument {
	return {
		...document,
		focusedLive: {
			...document.focusedLive,
			cameras: { ...document.focusedLive.cameras, [cameraId]: preference }
		}
	};
}

export function withMediaPreferences(
	document: PlaybackPreferenceDocument,
	media: Partial<PlaybackPreferenceDocument['media']>
): PlaybackPreferenceDocument {
	return { ...document, media: { ...document.media, ...media } };
}

function parsePlaybackPreferences(value: unknown): PlaybackPreferenceDocument {
	const fallback = defaultPlaybackPreferences();
	if (!isRecord(value) || value.version !== playbackPreferencesVersion) return fallback;
	const recorded = isRecord(value.recorded) ? value.recorded : {};
	const focusedLive = isRecord(value.focusedLive) ? value.focusedLive : {};
	const media = isRecord(value.media) ? value.media : {};
	return {
		version: playbackPreferencesVersion,
		recorded: {
			default: isRecordedPreference(recorded.default)
				? recorded.default
				: fallback.recorded.default,
			cameras: validPreferenceMap(recorded.cameras, isRecordedPreference)
		},
		focusedLive: {
			default: isFocusedLivePreference(focusedLive.default)
				? focusedLive.default
				: fallback.focusedLive.default,
			cameras: validPreferenceMap(focusedLive.cameras, isFocusedLivePreference)
		},
		media: {
			muted: typeof media.muted === 'boolean' ? media.muted : fallback.media.muted,
			playbackRate: isPlaybackRate(media.playbackRate)
				? media.playbackRate
				: fallback.media.playbackRate,
			playing: typeof media.playing === 'boolean' ? media.playing : fallback.media.playing
		}
	};
}

function validPreferenceMap<T extends string>(
	value: unknown,
	isPreference: (candidate: unknown) => candidate is T
): Record<string, T> {
	if (!isRecord(value)) return {};
	return Object.fromEntries(
		Object.entries(value).filter(
			(entry): entry is [string, T] => entry[0].length > 0 && isPreference(entry[1])
		)
	);
}

function isRecordedPreference(value: unknown): value is RecordedQualityPreference {
	return typeof value === 'string' && recordedPreferences.has(value as RecordedQualityPreference);
}

function isFocusedLivePreference(value: unknown): value is FocusedLivePreference {
	return typeof value === 'string' && focusedLivePreferences.has(value as FocusedLivePreference);
}

function isPlaybackRate(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value) && value >= 0.25 && value <= 8;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}
