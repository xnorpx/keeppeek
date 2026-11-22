import { capabilityActions } from './capability-actions';
import { MAX_EXPORT_DURATION_MS } from './keep-modes';
import type { ControlClient } from './control-client';
import type { ServerCapabilities } from './proto/webrtc_pb';
import type { ShareRecordingRange } from './proto/webrtc_pb';

export type RecordingRangeGuard = Pick<ShareRecordingRange, 'sourceId' | 'startMs' | 'endMs'>;
export type RecordingShareTarget = {
	sourceId: string;
	streamId?: 'main' | 'sub';
	startMs: number;
	endMs: number | null;
	bookmarkEventId?: string;
};
export type RangePreparation = {
	target: RecordingShareTarget;
	client: Pick<
		ControlClient,
		| 'getServerCapabilities'
		| 'queryStoredTimeline'
		| 'searchEventMetadata'
		| 'listExports'
		| 'getExport'
		| 'createExport'
	>;
};

export class RangePreparationError extends Error {}

const invalidInterval = 'Select a completed recording interval of up to two minutes.';
const variants = ['main', 'sub'] as const;
export const missingRangeProof = 'the completed export has no recorded media proof';

async function selectedEnd({ target, client }: RangePreparation, signal: AbortSignal) {
	const { sourceId, startMs } = target;
	if (
		!sourceId ||
		!Number.isSafeInteger(startMs) ||
		startMs < 0 ||
		startMs >= Number.MAX_SAFE_INTEGER
	)
		throw new RangePreparationError(invalidInterval);
	let endMs = target.endMs;
	if (target.bookmarkEventId) {
		endMs = null;
		for (const streamId of target.streamId ? [target.streamId] : variants) {
			signal.throwIfAborted();
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
			if (hit) {
				endMs = hit.endMs;
				break;
			}
		}
	}
	if (
		endMs === null ||
		!Number.isSafeInteger(endMs) ||
		endMs <= startMs ||
		endMs - startMs > MAX_EXPORT_DURATION_MS
	)
		throw new RangePreparationError(invalidInterval);
	return endMs;
}

async function retainedStream(
	{ target, client }: RangePreparation,
	endMs: number,
	caps: ServerCapabilities,
	signal: AbortSignal
): Promise<'main' | 'sub'> {
	const { sourceId, startMs } = target;
	if (target.streamId) return target.streamId;
	const stored = caps.storedMediaSources.find((source) => source.sourceId === sourceId);
	const timeline = await client.queryStoredTimeline({
		sourceIds: [sourceId],
		startMs,
		endMs,
		includeEvents: false,
		includeAttachments: false,
		signal
	});
	const candidates = variants
		.map((streamId) => {
			const type = stored?.streams.find((stream) => stream.streamId === streamId)?.contentType;
			// The real file is inspected on the recorder. Prefer a known AVC track when
			// retained formats are reported, and exclude explicitly incompatible codecs.
			const knownAvc = /avc[13]|h\.?264/i.test(type ?? '');
			const incompatible = type?.includes('codecs=') && !knownAvc;
			const ranges = timeline.ranges
				.filter((range) => range.sourceId === sourceId && range.streamId === streamId)
				.sort((left, right) => left.startMs - right.startMs);
			let through = startMs,
				coverage = 0;
			for (const range of ranges) {
				const end = Math.min(endMs, range.endMs);
				coverage += Math.max(0, end - Math.max(through, startMs, range.startMs));
				through = Math.max(through, end);
			}
			return { streamId, knownAvc, coverage: incompatible ? 0 : coverage };
		})
		.filter((candidate) => candidate.coverage > 0)
		.sort((a, b) => Number(b.knownAvc) - Number(a.knownAvc) || b.coverage - a.coverage);
	if (!candidates[0])
		throw new RangePreparationError(
			'No compatible recording is available in the selected interval.'
		);
	return candidates[0].streamId;
}

export async function prepareRangeShare(
	preparation: RangePreparation,
	signal: AbortSignal,
	freshExport = false
) {
	const { target, client } = preparation;
	signal.throwIfAborted();
	const capabilities = await client.getServerCapabilities();
	signal.throwIfAborted();
	if (!capabilities.capabilityIds.includes(capabilityActions.shareRecordingRange.capability))
		throw new RangePreparationError('The recorder does not support exact recording-range links.');
	const { sourceId, startMs } = target;
	const endMs = await selectedEnd(preparation, signal);
	signal.throwIfAborted();
	const streamId = await retainedStream(preparation, endMs, capabilities, signal);
	signal.throwIfAborted();
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
			candidate.status === 'ready' &&
			candidate.expiresAtMs !== null &&
			candidate.expiresAtMs > Date.now()
	);
	const reused = job !== undefined;
	job ??= await client.createExport({
		sourceId,
		streamId,
		startMs,
		endMs,
		allowPartial: true,
		guestRangePreparation: true
	});
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
	return { exportJobId: job.id, guard, reused };
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
