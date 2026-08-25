import { describe, expect, it } from 'vitest';
import {
	defaultPlaybackPreferences,
	focusedLivePreference,
	loadPlaybackPreferences,
	playbackPreferencesStorageKey,
	recordedPreference,
	savePlaybackPreferences,
	withFocusedLivePreference,
	withMediaPreferences,
	withRecordedPreference,
	type PlaybackPreferenceStorage
} from './playback-preferences';

class MemoryStorage implements PlaybackPreferenceStorage {
	readonly values = new Map<string, string>();

	getItem(key: string): string | null {
		return this.values.get(key) ?? null;
	}

	setItem(key: string, value: string): void {
		this.values.set(key, value);
	}
}

describe('playback preferences', () => {
	it('keeps recorded and focused-live preferences separate per camera', () => {
		let document = defaultPlaybackPreferences();
		document = withRecordedPreference(document, 'front-door', 'sub');
		document = withFocusedLivePreference(document, 'front-door', 'high');

		expect(recordedPreference(document, 'front-door')).toBe('sub');
		expect(focusedLivePreference(document, 'front-door')).toBe('high');
		expect(recordedPreference(document, 'garage')).toBe('auto');
		expect(focusedLivePreference(document, 'garage')).toBe('auto');
	});

	it('persists media and quality choices across reloads', () => {
		const storage = new MemoryStorage();
		let document = withRecordedPreference(defaultPlaybackPreferences(), 'front-door', 'low');
		document = withFocusedLivePreference(document, 'front-door', 'main');
		document = withMediaPreferences(document, { muted: false, playbackRate: 2, playing: false });
		savePlaybackPreferences(storage, document);

		expect(loadPlaybackPreferences(storage)).toEqual(document);
	});

	it('ignores a stale document version', () => {
		const storage = new MemoryStorage();
		storage.setItem(
			playbackPreferencesStorageKey,
			JSON.stringify({ version: 0, recorded: { default: 'sub' } })
		);

		expect(loadPlaybackPreferences(storage)).toEqual(defaultPlaybackPreferences());
	});

	it('drops invalid camera preferences and media values safely', () => {
		const storage = new MemoryStorage();
		storage.setItem(
			playbackPreferencesStorageKey,
			JSON.stringify({
				version: 1,
				recorded: { default: 'turbo', cameras: { valid: 'main', stale: 'ultra' } },
				focusedLive: { default: 'low', cameras: { valid: 'sub', stale: 42 } },
				media: { muted: 'yes', playbackRate: 100 }
			})
		);

		const document = loadPlaybackPreferences(storage);
		expect(document.recorded).toEqual({ default: 'auto', cameras: { valid: 'main' } });
		expect(document.focusedLive).toEqual({ default: 'low', cameras: { valid: 'sub' } });
		expect(document.media).toEqual({ muted: false, playbackRate: 1, playing: true });
	});

	it('does not share preferences between device-local storage instances', () => {
		const desktop = new MemoryStorage();
		const phone = new MemoryStorage();
		savePlaybackPreferences(
			desktop,
			withRecordedPreference(defaultPlaybackPreferences(), 'front-door', 'high')
		);
		savePlaybackPreferences(
			phone,
			withRecordedPreference(defaultPlaybackPreferences(), 'front-door', 'low')
		);

		expect(recordedPreference(loadPlaybackPreferences(desktop), 'front-door')).toBe('high');
		expect(recordedPreference(loadPlaybackPreferences(phone), 'front-door')).toBe('low');
	});
});
