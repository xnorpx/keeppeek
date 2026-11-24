import type { ShareEvidence } from './proto/webrtc_pb';
import {
	alternateRangeStream,
	CorrectedRangeStream,
	FailedRangeExport,
	guestExportByteLimitError,
	prepareRangeShare,
	RangePreparationError,
	replaceableRangeArtifact,
	waitForRangeExport,
	type PreparedRange,
	type RangePreparation,
	type RecordingRangeGuard
} from './sharing-range';

type Selection = {
	phase: 'waiting' | 'ready';
	job: PreparedRange;
	fallback: 'available' | 'exhausted';
};

type Preparation = 'initial' | 'alternate' | 'replacement';
export type PreviewAction = 'reload' | 'alternate' | 'replacement';

export class RangeSharePreview {
	#selection?: Selection;

	constructor(
		private readonly preview: (jobId: string, guard: RecordingRangeGuard) => Promise<ShareEvidence>
	) {}

	recovery(cause: unknown): Exclude<PreviewAction, 'reload'> | undefined {
		if (cause instanceof FailedRangeExport && cause.message === guestExportByteLimitError)
			return this.#selection?.fallback === 'available' ? 'alternate' : undefined;
		if (
			cause instanceof FailedRangeExport ||
			(this.#selection?.phase === 'ready' &&
				replaceableRangeArtifact(cause, this.#selection.job.reused))
		)
			return 'replacement';
	}

	async load(range: RangePreparation, action: PreviewAction, signal: AbortSignal) {
		let selected = this.#selection;
		if (!selected || action !== 'reload') {
			selected = await this.prepare(range, action === 'reload' ? 'initial' : action, signal);
		} else if (selected.phase === 'waiting') {
			await waitForRangeExport(range.client, selected.job, signal);
			selected.phase = 'ready';
		}
		for (;;) {
			signal.throwIfAborted();
			const { exportJobId, guard } = selected.job;
			try {
				return { evidence: await this.preview(exportJobId, guard), guard };
			} catch (cause) {
				if (selected.fallback === 'exhausted' || !alternateRangeStream(cause)) throw cause;
				try {
					selected = await this.prepare(range, 'alternate', signal);
				} catch (alternate) {
					if (this.#selection === selected && alternate instanceof RangePreparationError) {
						selected.fallback = 'exhausted';
						throw cause;
					}
					throw alternate;
				}
			}
		}
	}

	private async prepare(
		range: RangePreparation,
		kind: Preparation,
		signal: AbortSignal
	): Promise<Selection> {
		const previous = this.#selection?.job;
		const target = previous
			? {
					sourceId: previous.guard.sourceId,
					startMs: Number(previous.guard.startMs),
					endMs: Number(previous.guard.endMs),
					streamId: kind === 'replacement' ? previous.streamId : range.target.streamId
				}
			: range.target;
		let fallback: Selection['fallback'] =
			kind === 'replacement'
				? (this.#selection?.fallback ?? 'exhausted')
				: kind === 'initial' && !target.streamId
					? 'available'
					: 'exhausted';
		const remember = (job: PreparedRange) => {
			this.#selection = { phase: 'waiting', job, fallback };
		};
		let prepared: PreparedRange;
		try {
			prepared = await prepareRangeShare(
				{ client: range.client, target },
				signal,
				kind === 'replacement',
				kind === 'alternate' ? previous?.streamId : undefined,
				remember
			);
		} catch (cause) {
			if (!(cause instanceof CorrectedRangeStream) || fallback !== 'available') throw cause;
			fallback = 'exhausted';
			const { sourceId, startMs, endMs } = cause.guard;
			prepared = await prepareRangeShare(
				{
					client: range.client,
					target: { sourceId, startMs: Number(startMs), endMs: Number(endMs) }
				},
				signal,
				false,
				cause.streamId,
				remember
			);
		}
		this.#selection = { phase: 'ready', job: prepared, fallback };
		return this.#selection;
	}
}
