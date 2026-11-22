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
		'getServerCapabilities' | 'queryStoredTimeline' | 'listExports' | 'getExport' | 'createExport'
	>;
};

export class RangePreparationError extends Error {}

const invalidInterval = 'Select a completed recording interval of up to two minutes.';
const variants = ['main', 'sub'] as const;
export const missingRangeProof = 'the completed export has no recorded media proof';
export const incompatibleRangeCodec =
	'the selected recording could not be verified as browser-compatible';
export function replaceableRangeArtifact(cause: unknown): boolean {
	return (
		cause instanceof Error &&
		[
			missingRangeProof,
			incompatibleRangeCodec,
			'recorded audio correction prevents verification of the selected source interval',
			'the recording cannot be shared within the selected range'
		].includes(cause.message)
	);
}

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
		const page = await client.queryStoredTimeline({
			sourceIds: [sourceId],
			startMs,
			endMs: startMs + 1,
			includeEvents: true,
			includeAvailability: false,
			includeAttachments: false,
			signal
		});
		const hit = page.events.find(
			(event) =>
				event.id === target.bookmarkEventId &&
				event.source_id === sourceId &&
				event.start_time_ms === startMs
		);
		endMs = hit?.end_time_ms ?? null;
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
	signal: AbortSignal,
	excludedStream?: 'main' | 'sub'
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
		.filter((streamId) => streamId !== excludedStream)
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
	freshExport = false,
	excludedStream?: 'main' | 'sub'
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
	const guard = { sourceId, startMs: BigInt(startMs), endMs: BigInt(endMs) };
	const jobs = freshExport ? [] : await client.listExports();
	signal.throwIfAborted();
	const existing = jobs
		.filter(
			(candidate) =>
				candidate.sourceId === sourceId &&
				(!target.streamId || candidate.streamId === target.streamId) &&
				candidate.streamId !== excludedStream &&
				candidate.requestedStartMs === startMs &&
				candidate.requestedEndMs === endMs &&
				!candidate.burnInTimestamp &&
				candidate.status === 'ready' &&
				candidate.expiresAtMs !== null &&
				candidate.expiresAtMs > Date.now()
		)
		.sort(
			(left, right) =>
				(right.expiresAtMs ?? 0) - (left.expiresAtMs ?? 0) || left.id.localeCompare(right.id)
		)[0];
	if (existing)
		return { exportJobId: existing.id, streamId: existing.streamId, guard, reused: true };
	const streamId = await retainedStream(preparation, endMs, capabilities, signal, excludedStream);
	signal.throwIfAborted();
	let job = await client.createExport({
		sourceId,
		streamId,
		startMs,
		endMs,
		allowPartial: true,
		guestRangePreparation: true
	});
	const deadline = performance.now() + 5 * 60_000;
	while (job.status === 'running' && performance.now() < deadline) {
		signal.throwIfAborted();
		await new Promise<void>((resolve) => setTimeout(resolve, 500));
		signal.throwIfAborted();
		job = await client.getExport(job.id);
	}
	signal.throwIfAborted();
	if (job.status !== 'ready')
		throw new RangePreparationError('A completed export of this recording range is not available.');
	return { exportJobId: job.id, streamId, guard, reused: false };
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
