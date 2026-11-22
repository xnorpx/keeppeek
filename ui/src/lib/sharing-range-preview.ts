import type { ShareEvidence } from './proto/webrtc_pb';
import {
	alternateRangeStream,
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
			(this.#selection?.job.reused && replaceableRangeArtifact(cause))
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
		const fallback = kind === 'initial' && !target.streamId ? 'available' : 'exhausted';
		const prepared = await prepareRangeShare(
			{ client: range.client, target },
			signal,
			kind === 'replacement',
			kind === 'alternate' ? previous?.streamId : undefined,
			(job) => {
				this.#selection = { phase: 'waiting', job, fallback };
			}
		);
		this.#selection = { phase: 'ready', job: prepared, fallback };
		return this.#selection;
	}
}
