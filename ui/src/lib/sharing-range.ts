import { capabilityActions } from './capability-actions';
import { MAX_EXPORT_DURATION_MS } from './keep-modes';
import type { ControlClient } from './control-client';
import type { ShareRecordingRange } from './proto/webrtc_pb';

export type RecordingRangeGuard = Pick<ShareRecordingRange, 'sourceId' | 'startMs' | 'endMs'>;
export type RecordingShareTarget = {
	sourceId: string;
	streamId: 'main' | 'sub';
	startMs: number;
	endMs: number | null;
	bookmarkEventId?: string;
};
export type RangePreparation = {
	target: RecordingShareTarget;
	client: Pick<
		ControlClient,
		'getServerCapabilities' | 'searchEventMetadata' | 'listExports' | 'getExport' | 'createExport'
	>;
};

export class RangePreparationError extends Error {}

export async function prepareRangeShare(
	{ target, client }: RangePreparation,
	signal: AbortSignal,
	freshExport = false
) {
	signal.throwIfAborted();
	const capabilities = await client.getServerCapabilities();
	signal.throwIfAborted();
	if (!capabilities.capabilityIds.includes(capabilityActions.shareRecordingRange.capability))
		throw new RangePreparationError('The recorder does not support exact recording-range links.');
	const { sourceId, streamId, startMs } = target;
	if (
		!sourceId ||
		!Number.isSafeInteger(startMs) ||
		startMs < 0 ||
		startMs >= Number.MAX_SAFE_INTEGER
	)
		throw new RangePreparationError('Select a completed recording interval of up to two minutes.');
	let endMs = target.endMs;
	if (target.bookmarkEventId) {
		const page = await client.searchEventMetadata({
			sourceIds: [sourceId],
			eventIds: [target.bookmarkEventId],
			streamId,
			startMs,
			endMs: startMs + 1,
			pageSize: 1,
			signal
		});
		const hit = page.hits.find(
			(event) =>
				event.eventId === target.bookmarkEventId &&
				event.sourceId === sourceId &&
				event.startMs === startMs
		);
		endMs = hit?.endMs ?? null;
	}
	signal.throwIfAborted();
	if (
		endMs === null ||
		!Number.isSafeInteger(endMs) ||
		endMs <= startMs ||
		endMs - startMs > MAX_EXPORT_DURATION_MS
	)
		throw new RangePreparationError('Select a completed recording interval of up to two minutes.');
	const guard = { sourceId, startMs: BigInt(startMs), endMs: BigInt(endMs) };
	const jobs = freshExport ? [] : await client.listExports();
	signal.throwIfAborted();
	let job = jobs.find(
		(candidate) =>
			candidate.sourceId === sourceId &&
			candidate.streamId === streamId &&
			candidate.requestedStartMs === startMs &&
			candidate.requestedEndMs === endMs &&
			!candidate.burnInTimestamp &&
			(candidate.status === 'running' ||
				(candidate.status === 'ready' &&
					candidate.expiresAtMs !== null &&
					candidate.expiresAtMs > Date.now()))
	);
	job ??= await client.createExport({ sourceId, streamId, startMs, endMs, allowPartial: true });
	const deadline = performance.now() + 60_000;
	while (job.status === 'running' && performance.now() < deadline) {
		signal.throwIfAborted();
		await new Promise<void>((resolve) => setTimeout(resolve, 500));
		signal.throwIfAborted();
		job = await client.getExport(job.id);
	}
	signal.throwIfAborted();
	if (job.status !== 'ready')
		throw new RangePreparationError('A completed export of this recording range is not available.');
	return { exportJobId: job.id, guard };
}

export function insideRecordingRange(
	evidence: { sourceId: string; startMs: bigint; endMs: bigint },
	guard: RecordingRangeGuard
) {
	return (
		guard.startMs >= 0n &&
		guard.endMs > guard.startMs &&
		guard.endMs - guard.startMs <= BigInt(MAX_EXPORT_DURATION_MS) &&
		evidence.sourceId === guard.sourceId &&
		evidence.startMs >= guard.startMs &&
		evidence.endMs > evidence.startMs &&
		evidence.endMs <= guard.endMs
	);
}
