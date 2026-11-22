import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import { RecordedAudioSettingsSchema } from './proto/webrtc_pb';
import {
	effectiveRecordedAudioOffset,
	parseRecordedAudioOffset,
	recordedAudioSettings,
	protoRecordedAudioSettings
} from './recorded-audio';

describe('recorded audio settings', () => {
	it('preserves unsupported, reset, inherited and explicit-zero settings', () => {
		expect(recordedAudioSettings(undefined)).toBeUndefined();
		expect(protoRecordedAudioSettings(undefined)).toBeUndefined();
		const reset = protoRecordedAudioSettings({});
		expect(reset).toBeDefined();
		expect(reset?.defaultOffsetMs).toBeUndefined();
		const settings = recordedAudioSettings(
			create(RecordedAudioSettingsSchema, {
				defaultOffsetMs: 250,
				mainOffsetMs: 0
			})
		);
		expect(settings).toEqual({
			default_offset_ms: 250,
			main_offset_ms: 0,
			sub_offset_ms: undefined
		});
		expect(effectiveRecordedAudioOffset(settings!, 'main')).toBe(0);
		expect(effectiveRecordedAudioOffset(settings!, 'sub')).toBe(250);
		expect(effectiveRecordedAudioOffset({}, 'main')).toBe(0);
		expect(protoRecordedAudioSettings(settings)).toMatchObject({
			defaultOffsetMs: 250,
			mainOffsetMs: 0
		});
	});

	it.each(['-5000', '5000', '0', '+250'])(
		'accepts whole offsets at the approved bounds: %s',
		(input) => {
			expect(parseRecordedAudioOffset(input)).toBe(Number(input));
		}
	);

	it.each(['', '1.5', '-5001', '5001', 'Infinity', '1e3'])('rejects invalid offset %s', (input) => {
		expect(() => parseRecordedAudioOffset(input)).toThrow('whole number from -5000 to +5000 ms');
	});
});
