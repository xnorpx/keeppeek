import { describe, expect, it, vi } from 'vitest';
import {
	insideRecordingRange,
	prepareRangeShare,
	type RecordingShareTarget
} from './sharing-range';

const target: RecordingShareTarget = {
	sourceId: 'front',
	streamId: 'sub',
	startMs: 1000,
	endMs: 2000
};
const guard = { sourceId: 'front', startMs: 1000n, endMs: 2000n };
const ready = {
	id: 'recording-export',
	sourceId: 'front',
	streamId: 'sub',
	requestedStartMs: 1000,
	requestedEndMs: 2000,
	burnInTimestamp: false,
	expiresAtMs: Date.now() + 120_000,
	status: 'ready'
};
function client() {
	return {
		getServerCapabilities: vi
			.fn()
			.mockResolvedValue({ capabilityIds: ['keeppeek.guest-range.v1'] }),
		searchEventMetadata: vi.fn(),
		listExports: vi.fn().mockResolvedValue([]),
		createExport: vi.fn().mockResolvedValue(ready),
		getExport: vi.fn()
	};
}
const signal = () => new AbortController().signal;

describe('recording range sharing', () => {
	it('reuses only the unexpired, exact recording and keeps existing stream selection', async () => {
		const recorder = client();
		recorder.listExports.mockResolvedValue([
			{ ...ready, id: 'other', streamId: 'main' },
			{ ...ready, id: 'expired', expiresAtMs: 0 },
			{ ...ready, id: 'nearby', requestedEndMs: 1999 },
			ready
		]);
		expect(await prepareRangeShare({ client: recorder, target }, signal())).toEqual({
			exportJobId: ready.id,
			guard
		});
		expect(recorder.createExport).not.toHaveBeenCalled();
	});
	it('can explicitly replace a legacy match without extending its selection', async () => {
		const recorder = client();
		recorder.listExports.mockResolvedValue([{ ...ready, id: 'legacy' }]);
		const result = await prepareRangeShare({ client: recorder, target }, signal(), true);
		expect(result).toEqual({ exportJobId: ready.id, guard });
		expect(recorder.listExports).not.toHaveBeenCalled();
		expect(recorder.createExport).toHaveBeenCalledWith({ ...target, allowPartial: true });
	});

	it('prepares a new export using only the selected bookmark event and interval', async () => {
		const recorder = client();
		recorder.searchEventMetadata.mockResolvedValue({ hits: [{ eventId: 'saved', ...target }] });
		expect(
			await prepareRangeShare(
				{ client: recorder, target: { ...target, endMs: null, bookmarkEventId: 'saved' } },
				signal()
			)
		).toEqual({ exportJobId: ready.id, guard });
		expect(recorder.searchEventMetadata).toHaveBeenCalledWith(
			expect.objectContaining({
				eventIds: ['saved'],
				sourceIds: ['front'],
				startMs: 1000,
				endMs: 1001,
				streamId: 'sub'
			})
		);
		expect(recorder.createExport).toHaveBeenCalledWith({ ...target, allowPartial: true });
	});
	it('does not prepare a different or missing bookmark, an open event or an oversized interval', async () => {
		const recorder = client();
		recorder.searchEventMetadata.mockResolvedValue({
			hits: [
				{ eventId: 'other', ...target },
				{ eventId: 'saved', ...target, startMs: 900 }
			]
		});
		for (const selection of [
			{ ...target, endMs: null, bookmarkEventId: 'saved' },
			{ ...target, endMs: null },
			{ ...target, endMs: 121_001 }
		]) {
			await expect(
				prepareRangeShare({ client: recorder, target: selection }, signal())
			).rejects.toThrow('completed recording interval');
		}
		expect(recorder.listExports).not.toHaveBeenCalled();
		expect(recorder.createExport).not.toHaveBeenCalled();
	});
	it('keeps a legacy recorder and a closed dialog from preparing an export', async () => {
		const recorder = client();
		recorder.getServerCapabilities.mockResolvedValue({
			capabilityIds: ['keeppeek.guest-review.v1']
		});
		await expect(prepareRangeShare({ client: recorder, target }, signal())).rejects.toThrow(
			'does not support'
		);
		recorder.getServerCapabilities.mockClear();
		await expect(
			prepareRangeShare({ client: recorder, target }, AbortSignal.abort())
		).rejects.toThrow();
		expect(recorder.getServerCapabilities).not.toHaveBeenCalled();
		expect(recorder.listExports).not.toHaveBeenCalled();
	});
	it('accepts contained coverage and rejects crossed boundaries and empty evidence', () => {
		for (const measured of [guard, { ...guard, startMs: 1001n, endMs: 1999n }])
			expect(insideRecordingRange(measured, guard)).toBe(true);
		for (const measured of [
			{ ...guard, startMs: 999n },
			{ ...guard, endMs: 2001n },
			{ ...guard, endMs: 1000n },
			{ ...guard, sourceId: 'other' }
		])
			expect(insideRecordingRange(measured, guard)).toBe(false);
	});
});
