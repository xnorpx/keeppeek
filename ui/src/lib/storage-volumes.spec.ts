import { describe, expect, it } from 'vitest';
import { create } from '@bufbuild/protobuf';
import {
	StorageVolumeConfigurationSchema,
	StorageVolumeRole,
	StorageVolumeState
} from './proto/webrtc_pb';
import { encodeDraft, newVolume, parseBytes, volumeDraft } from './storage-volumes';

describe('named storage drafts', () => {
	it('round trips exact byte integers and unresolved secret references', () => {
		const value = create(StorageVolumeConfigurationSchema, {
			volumes: [
				{
					id: '{secret:ID}',
					root: '{secret:ROOT}',
					roles: [StorageVolumeRole.ARCHIVE],
					state: StorageVolumeState.DISABLED,
					capacityBytes: 9000000000000000001n
				}
			]
		});
		expect(encodeDraft(volumeDraft(value))).toEqual(value);
	});
	it('rejects rounded/exponent/negative/out-of-range byte input', () => {
		for (const value of ['9e18', '-1', '1.5', '9223372036854775808'])
			expect(() => parseBytes(value, 'Capacity')).toThrow();
		expect(parseBytes('9007199254740993', 'Capacity')).toBe(9007199254740993n);
	});
	it('rejects dangling policies and preserves deliberate empty configurations', () => {
		expect(encodeDraft({ volumes: [], placement: [] }).volumes).toEqual([]);
		expect(() =>
			encodeDraft({
				volumes: [{ ...newVolume(), id: 'archive', root: '/archive' }],
				placement: [
					{
						role: StorageVolumeRole.ARCHIVE,
						source: '',
						group: '',
						candidates: 'missing',
						strategy: 1,
						allowFallback: false
					}
				]
			})
		).toThrow('candidate');
	});
});
