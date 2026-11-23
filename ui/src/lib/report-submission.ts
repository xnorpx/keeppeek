import schema from '../../../schemas/reporting/report-v1.schema.json';
import type { ReportLogWindow } from './report-log-window';

export type ReportText = {
	title: string;
	description: string;
	reproduction: string;
	expected: string;
	actual: string;
};
export const reportTextFields = [
	{ key: 'title', label: 'Issue title', limit: schema.properties.title.maxLength },
	{
		key: 'description',
		label: 'Problem description',
		limit: schema.properties.description.maxLength
	},
	{
		key: 'reproduction',
		label: 'Steps to reproduce',
		limit: schema.properties.reproduction.maxLength
	},
	{ key: 'expected', label: 'Expected behavior', limit: schema.properties.expected.maxLength },
	{ key: 'actual', label: 'Actual behavior', limit: schema.properties.actual.maxLength }
] as const;

type ArtifactKind = 'diagnostics' | 'screen';
type ArtifactDescription = { type: string; bytes: number };
type Manifest = ReportText & {
	version: 1;
	evidenceWarnings: readonly string[];
	artifacts: { diagnostics: ArtifactDescription; screen?: ArtifactDescription };
};
export type ReportGrant = { id: string; token: string };
export type ReportState =
	{ state: 'pending' | 'cancelled' | 'failed' } | { state: 'complete'; issueUrl: string };
export type ReportProgress = { uploaded: number; total: number };

export interface ReportTransport {
	create(manifest: Manifest, idempotencyKey: string, signal: AbortSignal): Promise<ReportGrant>;
	status(grant: ReportGrant, signal: AbortSignal): Promise<ReportState>;
	upload(
		grant: ReportGrant,
		kind: ArtifactKind,
		artifact: Blob,
		signal: AbortSignal,
		onBytes: (count: number) => void
	): Promise<void>;
	finalize(grant: ReportGrant, signal: AbortSignal): Promise<ReportState>;
	cancel(grant: ReportGrant, signal: AbortSignal): Promise<void>;
}

function textDescription(input: ReportText): ReportText {
	const trimmed = { ...input };
	for (const { key, limit } of reportTextFields) {
		const value = input[key].trim();
		if (!value || [...value].length > limit) {
			throw new Error('Review every report field and its length before submitting.');
		}
		trimmed[key] = value;
	}
	return trimmed;
}

function artifactDescription(kind: ArtifactKind, artifact: Blob): ArtifactDescription {
	const rules = schema.$defs[kind].properties;
	if (
		!rules.type.enum.includes(artifact.type) ||
		artifact.size < rules.bytes.minimum ||
		artifact.size > rules.bytes.maximum
	) {
		throw new Error('The report contains an unsupported or oversized artifact.');
	}
	return { type: artifact.type, bytes: artifact.size };
}

type ReviewInput = {
	text: ReportText;
	diagnostics: Blob;
	screen?: Blob;
	evidenceWarnings: ReportLogWindow['manifest']['warnings'];
};

export class ReportSubmission {
	readonly #manifest: Manifest;
	readonly #artifacts: ReadonlyArray<readonly [ArtifactKind, Blob]>;
	readonly #total: number;
	readonly #key: string;
	#active: { controller: AbortController; result: Promise<ReportState> } | null = null;
	#cancelled = false;
	#grant: ReportGrant | undefined;
	#completed: Extract<ReportState, { state: 'complete' }> | undefined;

	constructor(
		private readonly transport: ReportTransport,
		review: ReviewInput
	) {
		const diagnostics = artifactDescription('diagnostics', review.diagnostics);
		const screen = review.screen ? artifactDescription('screen', review.screen) : undefined;
		this.#artifacts = [
			['diagnostics', review.diagnostics],
			...(review.screen ? [['screen', review.screen] as const] : [])
		];
		this.#total = diagnostics.bytes + (screen?.bytes ?? 0);
		this.#manifest = {
			version: 1,
			...textDescription(review.text),
			evidenceWarnings: [...review.evidenceWarnings],
			artifacts: { diagnostics, ...(screen ? { screen } : {}) }
		};
		this.#key = crypto.randomUUID();
	}

	submit(onProgress: (progress: ReportProgress) => void): Promise<ReportState> {
		if (this.#completed) return Promise.resolve(this.#completed);
		if (this.#cancelled) return Promise.resolve({ state: 'cancelled' });
		if (this.#active) return this.#active.result;
		const controller = new AbortController();
		const result = this.run(controller.signal, onProgress);
		this.#active = { controller, result };
		void result.then(
			() => this.clear(controller),
			() => this.clear(controller)
		);
		return result;
	}

	private clear(controller: AbortController): void {
		if (this.#active?.controller === controller) this.#active = null;
	}

	private async run(
		signal: AbortSignal,
		onProgress: (progress: ReportProgress) => void
	): Promise<ReportState> {
		// The reviewed payload and its key are fixed for this submission, including retries.
		const grant = await this.transport.create(this.#manifest, this.#key, signal);
		if (signal.aborted) {
			await this.cancelStaged(grant);
			signal.throwIfAborted();
		}
		this.#grant = grant;
		const status = await this.transport.status(grant, signal);
		signal.throwIfAborted();
		if (status.state !== 'pending') return this.finish(status);
		let committed = 0;
		onProgress({ uploaded: committed, total: this.#total });
		for (const [kind, artifact] of this.#artifacts) {
			await this.transport.upload(grant, kind, artifact, signal, (count) => {
				if (!signal.aborted && Number.isFinite(count)) {
					onProgress({
						uploaded: committed + Math.min(artifact.size, Math.max(0, count)),
						total: this.#total
					});
				}
			});
			signal.throwIfAborted();
			committed += artifact.size;
			onProgress({ uploaded: committed, total: this.#total });
		}
		const finalized = await this.transport.finalize(grant, signal);
		signal.throwIfAborted();
		return this.finish(finalized);
	}

	private finish(state: ReportState): ReportState {
		if (state.state === 'complete') {
			this.#completed = state;
			this.#grant = undefined;
		}
		return state;
	}

	async cancel(): Promise<void> {
		this.#cancelled = true;
		this.#active?.controller.abort();
		const grant = this.#grant;
		this.#grant = undefined;
		if (grant) await this.cancelStaged(grant);
	}

	private async cancelStaged(grant: ReportGrant): Promise<void> {
		// Upload cancellation is immediate. A bounded second request removes accepted chunks.
		await this.transport.cancel(grant, AbortSignal.timeout(5_000)).catch(() => undefined);
	}
}
