import { create } from '@bufbuild/protobuf';
import { describe, expect, it, vi } from 'vitest';
import {
	archiveDraft,
	archiveJobId,
	encodeArchiveDraft,
	submitArchiveCopy,
	type ArchiveController
} from './archive';
import { ArchiveResultSchema, ArchiveSettingsSchema, StorageObjectKind } from './proto/webrtc_pb';

describe('local archive workflow', () => {
	it('creates canonical v4 job IDs without the secure-context randomUUID API', () => {
		const getRandomValues = vi.fn((bytes: Uint8Array) => bytes.fill(255));
		expect(archiveJobId({ getRandomValues })).toBe('ffffffff-ffff-4fff-bfff-ffffffffffff');
		expect(getRandomValues).toHaveBeenCalledOnce();
		expect(getRandomValues.mock.calls[0]?.[0]).toHaveLength(16);
	});
	it('explains how to recover when cryptographic randomness is unavailable', () => {
		expect(() => archiveJobId({})).toThrow('HTTPS or localhost');
	});
	it('keeps private references and exact byte budgets in editable drafts', () => {
		const settings = create(ArchiveSettingsSchema, {
			targetId: '{secret:TARGET}',
			root: '{secret:ROOT}',
			sourceIds: ['{secret:SOURCE}'],
			maximumObjectBytes: 68_719_476_736n,
			maximumPendingBytes: 9_000_000_000_000_000_000n,
			maximumPendingCount: 256,
			operationTimeoutSecs: 120n,
			maximumAttempts: 3
		});
		expect(encodeArchiveDraft(archiveDraft(settings))).toEqual(settings);
	});
	it('bounds transfer work and requires explicit eviction opt in', () => {
		const draft = archiveDraft();
		draft.root = '/archive';
		expect(encodeArchiveDraft(draft)).toMatchObject({
			enabled: false,
			automatic: false,
			evictAfterArchive: false
		});
		draft.maximumObjectBytes = '68719476737';
		expect(() => encodeArchiveDraft(draft)).toThrow('Maximum object bytes');
		draft.maximumObjectBytes = '1';
		draft.maximumPendingCount = '4097';
		expect(() => encodeArchiveDraft(draft)).toThrow('Maximum pending jobs');
		draft.maximumPendingCount = '1';
		draft.sourceIds = 'same\nsame';
		expect(() => encodeArchiveDraft(draft)).toThrow('unique');
	});
	it('reconciles a lost copy acknowledgment using the original durable job identity', async () => {
		const jobId = archiveJobId({ getRandomValues: (bytes) => bytes.fill(1) });
		const archive = vi
			.fn<ArchiveController['archive']>()
			.mockRejectedValueOnce(new Error('connection lost'))
			.mockResolvedValueOnce(
				create(ArchiveResultSchema, {
					result: { case: 'job', value: { jobId, state: 'copying' } }
				})
			);
		const result = await submitArchiveCopy(
			{ archive },
			{ id: 'recording', kind: StorageObjectKind.RECORDING },
			jobId
		);
		expect(result.jobId).toBe(jobId);
		expect(archive.mock.calls.map(([request]) => request.action)).toMatchObject([
			{ case: 'copy', value: { jobId, objectId: 'recording' } },
			{ case: 'get', value: { jobId } }
		]);
	});
});
