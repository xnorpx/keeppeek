import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import {
	OpenStoredMediaSchema,
	RecordedAudioSettingsSchema,
	RecordedAudioTimingSchema,
	SetStoredMediaPlaybackSchema,
	StoredMediaStateSchema,
	UpdateCameraConfigurationSchema
} from './proto/webrtc_pb';

describe('recorded-audio additive wire contract', () => {
	it('distinguishes unchanged settings, reset, inheritance, and explicit zero', () => {
		const unchanged = fromBinary(UpdateCameraConfigurationSchema, new Uint8Array());
		expect(unchanged.recordedAudio).toBeUndefined();
		const reset = create(UpdateCameraConfigurationSchema, { recordedAudio: {} });
		const decodedReset = fromBinary(
			UpdateCameraConfigurationSchema,
			toBinary(UpdateCameraConfigurationSchema, reset)
		);
		expect(decodedReset.recordedAudio).toBeDefined();
		expect(decodedReset.recordedAudio?.defaultOffsetMs).toBeUndefined();
		const saved = create(RecordedAudioSettingsSchema, {
			defaultOffsetMs: -250,
			mainOffsetMs: 0
		});
		const decoded = fromBinary(
			RecordedAudioSettingsSchema,
			toBinary(RecordedAudioSettingsSchema, saved)
		);
		expect(decoded.defaultOffsetMs).toBe(-250);
		expect(decoded.mainOffsetMs).toBe(0);
		expect(decoded.subOffsetMs).toBeUndefined();
	});

	it('keeps a zero preview distinct from configured timing and omitted changes', () => {
		const request = create(OpenStoredMediaSchema, {
			recordedAudioPreview: { selection: { case: 'offsetMs', value: 0 } }
		});
		const decoded = fromBinary(OpenStoredMediaSchema, toBinary(OpenStoredMediaSchema, request));
		expect(decoded.recordedAudioPreview?.selection).toEqual({ case: 'offsetMs', value: 0 });
		const configured = create(SetStoredMediaPlaybackSchema, {
			recordedAudioPreview: { selection: { case: 'useConfigured', value: true } }
		});
		expect(
			fromBinary(SetStoredMediaPlaybackSchema, toBinary(SetStoredMediaPlaybackSchema, configured))
				.recordedAudioPreview?.selection
		).toEqual({ case: 'useConfigured', value: true });
		expect(
			fromBinary(SetStoredMediaPlaybackSchema, new Uint8Array()).recordedAudioPreview
		).toBeUndefined();
	});

	it('preserves unknown diagnostics versus inspected no-audio and zero clipping', () => {
		const unknown = fromBinary(RecordedAudioTimingSchema, new Uint8Array());
		expect(unknown.audioPresent).toBeUndefined();
		expect(unknown.clippedAudioSamples).toBeUndefined();
		expect(unknown.storedSkewUs).toBeUndefined();
		const state = create(StoredMediaStateSchema, {
			archive: true,
			recordedAudio: { audioPresent: false, clippedAudioSamples: 0, storedSkewUs: -1250n }
		});
		const decoded = fromBinary(StoredMediaStateSchema, toBinary(StoredMediaStateSchema, state));
		expect(decoded.archive).toBe(true);
		expect(decoded.recordedAudio?.audioPresent).toBe(false);
		expect(decoded.recordedAudio?.clippedAudioSamples).toBe(0);
		expect(decoded.recordedAudio?.storedSkewUs).toBe(-1250n);
	});
});
