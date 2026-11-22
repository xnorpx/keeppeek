import { create } from '@bufbuild/protobuf';
import {
	RecordedAudioSettingsSchema,
	type RecordedAudioSettings as ProtoRecordedAudioSettings
} from './proto/webrtc_pb';
import type { RecordedAudioSettings } from './types';

export function recordedAudioSettings(
	settings: ProtoRecordedAudioSettings | undefined
): RecordedAudioSettings | undefined {
	if (!settings) return undefined;
	return {
		default_offset_ms: settings.defaultOffsetMs,
		main_offset_ms: settings.mainOffsetMs,
		sub_offset_ms: settings.subOffsetMs
	};
}

export function protoRecordedAudioSettings(
	settings: RecordedAudioSettings | undefined
): ProtoRecordedAudioSettings | undefined {
	if (!settings) return undefined;
	return create(RecordedAudioSettingsSchema, {
		defaultOffsetMs: settings.default_offset_ms,
		mainOffsetMs: settings.main_offset_ms,
		subOffsetMs: settings.sub_offset_ms
	});
}

export function effectiveRecordedAudioOffset(
	settings: RecordedAudioSettings,
	stream: 'main' | 'sub'
): number {
	return settings[`${stream}_offset_ms`] ?? settings.default_offset_ms ?? 0;
}

export function parseRecordedAudioOffset(input: string): number {
	const value = Number(input.trim());
	if (!/^[+-]?\d+$/.test(input.trim()) || !Number.isInteger(value) || Math.abs(value) > 5_000) {
		throw new Error('Audio offset must be a whole number from -5000 to +5000 ms.');
	}
	return value;
}

export function formatRecordedAudioOffset(value: number): string {
	return `${value > 0 ? '+' : ''}${value} ms`;
}
