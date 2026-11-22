import recoveryMessages from '../../../tests/fixtures/guest-range-recovery.json';
import { describe, expect, it, vi } from 'vitest';
import { create } from '@bufbuild/protobuf';
import { ShareEvidenceSchema } from './proto/webrtc_pb';
import { RangeSharePreview } from './sharing-range-preview';
import {
	alternateRangeStream,
	replaceableRangeArtifact,
	insideRecordingRange,
	prepareRangeShare,
	type RecordingShareTarget
} from './sharing-range';

it('recognizes each recorder recovery message exactly', () => {
	const { incompatibleCodec, correctedAudio, crossedRange, missingProof } = recoveryMessages;
	for (const message of Object.values(recoveryMessages)) {
		const error = new Error(message);
		expect(alternateRangeStream(error)).toBe(
			[incompatibleCodec, correctedAudio, crossedRange].includes(message)
		);
		expect(replaceableRangeArtifact(error)).toBe([missingProof, correctedAudio].includes(message));
	}
});

const target: RecordingShareTarget = {
	sourceId: 'front',
	streamId: 'sub',
	startMs: 1000,
	endMs: 2000
};
const guard = { sourceId: 'front', startMs: 1000n, endMs: 2000n };
const ready = {
	id: 'recording-export',
	bytesWritten: 100,
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
		queryStoredTimeline: vi.fn(),
		getEventWorkflow: vi.fn().mockResolvedValue({
			states: [
				{
					sourceId: 'front',
					eventId: 'saved',
					eventPresent: true,
					bookmark: { active: true, eventStartMs: 1000 }
				}
			]
		}),
		listExports: vi.fn().mockResolvedValue([]),
		createExport: vi.fn().mockResolvedValue(ready),
		getExport: vi.fn()
	};
}
const signal = () => new AbortController().signal;
const savedEvent = { id: 'saved', source_id: 'front', start_time_ms: 1000, end_time_ms: 2000 };

describe('recording range sharing', () => {
	it('resumes a failed alternate preparation by its own job ID and guard on reload', async () => {
		const recorder = client();
		recorder.getServerCapabilities.mockResolvedValue({
			capabilityIds: ['keeppeek.guest-range.v1'],
			storedMediaSources: []
		});
		recorder.listExports.mockResolvedValue([{ ...ready, id: 'main-ready', streamId: 'main' }]);
		recorder.queryStoredTimeline.mockResolvedValue({ ranges: [{ ...target, streamId: 'sub' }] });
		const failed = {
			...ready,
			id: 'sub-job',
			status: 'failed',
			error: 'The recorder could not finish the selected export.'
		};
		recorder.createExport.mockResolvedValue(failed);
		recorder.getExport.mockResolvedValue({ ...failed, status: 'ready' });
		const evidence = create(ShareEvidenceSchema);
		const preview = vi
			.fn()
			.mockRejectedValueOnce(new Error(recoveryMessages.incompatibleCodec))
			.mockResolvedValue(evidence);
		const recovery = new RangeSharePreview(preview);
		const range = { client: recorder, target: { ...target, streamId: undefined } };
		await expect(recovery.load(range, 'reload', signal())).rejects.toThrow(failed.error);
		expect(recovery.recovery(new Error('Connection lost'))).toBeUndefined();
		expect(await recovery.load(range, 'reload', signal())).toEqual({ evidence, guard });
		expect(recorder.createExport).toHaveBeenCalledTimes(1);
		expect(recorder.getExport).toHaveBeenCalledWith('sub-job');
		expect(preview.mock.calls).toEqual([
			['main-ready', guard],
			['sub-job', guard]
		]);
	});

	it('reuses only the unexpired, exact recording and keeps existing stream selection', async () => {
		const recorder = client();
		recorder.listExports.mockResolvedValue([
			{ ...ready, id: 'other', streamId: 'main' },
			{ ...ready, id: 'unbounded-running-job', status: 'running' },
			{ ...ready, id: 'expired', expiresAtMs: 0 },
			{ ...ready, id: 'nearby', requestedEndMs: 1999 },
			ready
		]);
		expect(await prepareRangeShare({ client: recorder, target }, signal())).toEqual({
			exportJobId: ready.id,
			streamId: 'sub',
			reused: true,
			guard
		});
		expect(recorder.createExport).not.toHaveBeenCalled();
	});
	it('prefers an eligible substream over a newer over-budget completed main export', async () => {
		const recorder = client();
		recorder.listExports.mockResolvedValue([
			{
				...ready,
				id: 'large-main',
				streamId: 'main',
				expiresAtMs: ready.expiresAtMs + 1000,
				bytesWritten: 512 * 1024 * 1024 + 1
			},
			{ ...ready, bytesWritten: 512 * 1024 * 1024 }
		]);
		const selected = await prepareRangeShare(
			{ client: recorder, target: { ...target, streamId: undefined } },
			signal()
		);
		expect(selected.exportJobId).toBe(ready.id);
		expect(selected.guard).toEqual(guard);
	});

	it('rechecks a failed bounded job on reload and prepares another stream only on request', async () => {
		const recorder = client();
		recorder.getServerCapabilities.mockResolvedValue({
			capabilityIds: ['keeppeek.guest-range.v1'],
			storedMediaSources: []
		});
		recorder.queryStoredTimeline.mockResolvedValue({
			ranges: ['main', 'sub'].map((streamId) => ({ ...target, streamId }))
		});
		const failed = {
			...ready,
			id: 'large-main',
			streamId: 'main',
			status: 'failed',
			error: recoveryMessages.byteLimit
		};
		recorder.createExport.mockResolvedValueOnce(failed).mockResolvedValueOnce(ready);
		recorder.getExport.mockResolvedValue(failed);
		const preview = new RangeSharePreview(vi.fn().mockResolvedValue(create(ShareEvidenceSchema)));
		const range = { client: recorder, target: { ...target, streamId: undefined } };
		const first = await preview.load(range, 'reload', signal()).catch((cause: unknown) => cause);
		expect(preview.recovery(first)).toBe('alternate');
		await expect(preview.load(range, 'reload', signal())).rejects.toThrow(failed.error);
		expect(recorder.getExport).toHaveBeenCalledWith('large-main');
		expect(recorder.createExport).toHaveBeenCalledTimes(1);
		await expect(preview.load(range, 'alternate', signal())).resolves.toMatchObject({ guard });
		expect(
			recorder.createExport.mock.calls.map(([request]) => [
				request.streamId,
				request.startMs,
				request.endMs
			])
		).toEqual([
			['main', 1000, 2000],
			['sub', 1000, 2000]
		]);
		recorder.createExport.mockResolvedValue(failed);
		const explicit = new RangeSharePreview(vi.fn());
		const fixed = await explicit
			.load({ client: recorder, target: { ...target, streamId: 'main' } }, 'reload', signal())
			.catch((cause: unknown) => cause);
		expect(explicit.recovery(fixed)).toBeUndefined();
	});
	it('reuses the newest exact ready artifact even after the original footage is retained away', async () => {
		const recorder = client();
		recorder.listExports.mockResolvedValue([
			ready,
			{ ...ready, id: 'newest', streamId: 'main', expiresAtMs: ready.expiresAtMs + 1000 }
		]);
		const result = await prepareRangeShare(
			{ client: recorder, target: { ...target, streamId: undefined } },
			signal()
		);
		expect(result).toMatchObject({ exportJobId: 'newest', reused: true, guard });
		expect(recorder.queryStoredTimeline).not.toHaveBeenCalled();
	});

	it('continues polling the same bounded export beyond the first minute', async () => {
		const recorder = client();
		recorder.createExport.mockResolvedValue({ ...ready, status: 'running' });
		recorder.getExport.mockResolvedValue(ready);
		const clock = vi.spyOn(performance, 'now').mockReturnValueOnce(0).mockReturnValue(61000);
		try {
			expect(await prepareRangeShare({ client: recorder, target }, signal())).toMatchObject({
				exportJobId: ready.id,
				reused: false,
				guard
			});
			expect(recorder.getExport).toHaveBeenCalledTimes(1);
			expect(recorder.getExport).toHaveBeenCalledWith(ready.id);
		} finally {
			clock.mockRestore();
		}
	});

	it('can explicitly replace a legacy match without extending its selection', async () => {
		const recorder = client();
		recorder.listExports.mockResolvedValue([{ ...ready, id: 'legacy' }]);
		const result = await prepareRangeShare({ client: recorder, target }, signal(), true);
		expect(result).toEqual({ exportJobId: ready.id, streamId: 'sub', guard, reused: false });
		expect(recorder.listExports).not.toHaveBeenCalled();
		expect(recorder.createExport).toHaveBeenCalledWith({
			...target,
			allowPartial: true,
			guestRangePreparation: true
		});
	});

	it('prepares a new export using only the selected bookmark event and interval', async () => {
		const recorder = client();
		recorder.queryStoredTimeline.mockResolvedValue({ events: [savedEvent] });
		expect(
			await prepareRangeShare(
				{ client: recorder, target: { ...target, endMs: null, bookmarkEventId: 'saved' } },
				signal()
			)
		).toEqual({ exportJobId: ready.id, streamId: 'sub', guard, reused: false });
		expect(recorder.queryStoredTimeline).toHaveBeenCalledWith(
			expect.objectContaining({
				includeAvailability: false,
				includeEvents: true,
				sourceIds: ['front'],
				startMs: 1000,
				endMs: 1001
			})
		);
		expect(recorder.createExport).toHaveBeenCalledWith({
			...target,
			allowPartial: true,
			guestRangePreparation: true
		});
	});
	it('does not prepare a different or missing bookmark, an open event or an oversized interval', async () => {
		const recorder = client();
		recorder.queryStoredTimeline.mockResolvedValue({
			events: [
				{ ...savedEvent, id: 'other' },
				{ ...savedEvent, source_id: 'other' }
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
	it('resolves both bookmark edges after the event moves beyond its saved timestamp', async () => {
		const recorder = client();
		recorder.getEventWorkflow.mockResolvedValue({
			states: [
				{
					sourceId: 'front',
					eventId: 'saved',
					eventPresent: true,
					bookmark: { active: true, eventStartMs: 4000 }
				}
			]
		});
		recorder.queryStoredTimeline.mockResolvedValue({
			events: [{ ...savedEvent, start_time_ms: 4000, end_time_ms: 5400 }]
		});
		const prepared = await prepareRangeShare(
			{ client: recorder, target: { ...target, endMs: null, bookmarkEventId: 'saved' } },
			signal()
		);
		expect(recorder.getEventWorkflow).toHaveBeenCalledWith([
			{ sourceId: 'front', eventId: 'saved' }
		]);
		expect(recorder.queryStoredTimeline).toHaveBeenCalledWith(
			expect.objectContaining({ sourceIds: ['front'], startMs: 4000, endMs: 4001 })
		);
		expect(recorder.createExport).toHaveBeenCalledWith(
			expect.objectContaining({ startMs: 4000, endMs: 5400 })
		);
		expect(prepared.guard).toEqual({ sourceId: 'front', startMs: 4000n, endMs: 5400n });
	});
	it('selects retained compatible footage for an event, preserving both interval edges', async () => {
		const recorder = client();
		recorder.getServerCapabilities.mockResolvedValue({
			capabilityIds: ['keeppeek.guest-range.v1'],
			storedMediaSources: [
				{
					sourceId: 'front',
					streams: [
						{ streamId: 'main', contentType: 'video/mp4; codecs="hvc1.1.6.L93.B0"' },
						{ streamId: 'sub', contentType: 'video/mp4; codecs="avc1.42E01E"' }
					]
				}
			]
		});
		recorder.queryStoredTimeline.mockResolvedValue({
			events: [savedEvent],
			ranges: [
				{ sourceId: 'front', streamId: 'main', startMs: 1000, endMs: 2000 },
				{ sourceId: 'front', streamId: 'sub', startMs: 1100, endMs: 1900 }
			]
		});
		const { guard: selected } = await prepareRangeShare(
			{
				client: recorder,
				target: {
					sourceId: 'front',
					startMs: 1000,
					endMs: null,
					bookmarkEventId: 'saved'
				}
			},
			signal()
		);
		expect(selected).toEqual(guard);
		expect(recorder.queryStoredTimeline.mock.calls[0]![0]).toMatchObject({
			includeAvailability: false,
			includeEvents: true
		});
		expect(recorder.createExport).toHaveBeenCalledWith({
			...target,
			allowPartial: true,
			guestRangePreparation: true
		});
		recorder.queryStoredTimeline.mockResolvedValue({ ranges: [] });
		await expect(
			prepareRangeShare({ client: recorder, target: { ...target, streamId: undefined } }, signal())
		).rejects.toThrow('No compatible recording');
		expect(recorder.createExport).toHaveBeenCalledTimes(1);
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
