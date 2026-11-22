import { create } from '@bufbuild/protobuf';
import {
	LiveAudioSettingsSchema,
	RecordedAudioSettingsSchema,
	type RecordedAudioSettings as ProtoRecordedAudioSettings
} from './proto/webrtc_pb';
import type { RecordedAudioSettings } from './types';

export function recordedAudioSettings(
	settings:
		Pick<ProtoRecordedAudioSettings, 'defaultOffsetMs' | 'mainOffsetMs' | 'subOffsetMs'> | undefined
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

export function protoLiveAudioSettings(settings: RecordedAudioSettings | undefined) {
	if (!settings) return undefined;
	return create(LiveAudioSettingsSchema, {
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

export function parseRecordedAudioOffset(input: string, limit: 200 | 5000 = 5000): number {
	const value = Number(input.trim());
	if (!/^[+-]?\d+$/.test(input.trim()) || !Number.isInteger(value) || Math.abs(value) > limit) {
		throw new Error(`Audio offset must be a whole number from -${limit} to +${limit} ms.`);
	}
	return value;
}

export function formatRecordedAudioOffset(value: number): string {
	return `${value > 0 ? '+' : ''}${value} ms`;
}

export const timingEditorKinds = {
	live: {
		setting: 'live_audio',
		label: 'Live',
		limit: 200,
		description:
			'Positive offsets delay audio. Negative offsets delay video. Use whole milliseconds from −200 to +200. Explicit 0 disables correction at that level.',
		save: 'Save live timing',
		saved: 'Live timing saved.'
	},
	recorded: {
		setting: 'recorded_audio',
		label: 'Recorded',
		limit: 5000,
		description:
			'Positive offsets delay audio. Negative offsets advance it. Source recordings stay unchanged. Use whole milliseconds from −5000 to +5000. Explicit 0 disables correction at that level.',
		save: 'Save recorded timing',
		saved: 'Recorded timing saved. Camera recording continues.'
	}
} as const;
