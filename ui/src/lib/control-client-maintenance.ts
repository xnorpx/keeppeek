import { create } from '@bufbuild/protobuf';
import {
	RecordingDeletionJobSchema,
	RecordingMaintenanceCommandSchema,
	type RecordingDeletionJob,
	type RecordingDeletionReason,
	type RecordingMaintenanceCommand,
	type RecordingReconciliationReport,
	RecordingRemedy,
	type Ok,
	type Request
} from './proto/webrtc_pb';

export const RECORDING_MAINTENANCE_CAPABILITY = 'keeppeek.recording-maintenance.v1';

export type MaintenanceScope = { sourceId: string; streamId: string } & (
	{ recordingId: string } | { startMs: number; endMs: number }
);

export class RecordingMaintenanceClient {
	#confirmations = new WeakMap<RecordingDeletionJob, string>();

	constructor(private readonly send: (command: Request['command']) => Promise<Ok['result']>) {}

	discardConfirmations(): void {
		this.#confirmations = new WeakMap();
	}

	discardPreview(job: RecordingDeletionJob): void {
		this.#confirmations.delete(job);
	}

	async preview(
		scope: MaintenanceScope,
		reason: RecordingDeletionReason
	): Promise<RecordingDeletionJob> {
		if (!scope.sourceId.trim() || !scope.streamId.trim())
			throw new Error('Select a camera and stream.');
		if (
			!('recordingId' in scope) &&
			(!Number.isSafeInteger(scope.startMs) ||
				!Number.isSafeInteger(scope.endMs) ||
				scope.startMs < 0 ||
				scope.endMs <= scope.startMs ||
				scope.endMs - scope.startMs > 31 * 86_400_000)
		) {
			throw new Error('Select a nonempty UTC interval of at most 31 days.');
		}
		const command = create(RecordingMaintenanceCommandSchema, {
			action: {
				case: 'preview',
				value: {
					scope: {
						sourceId: scope.sourceId,
						streamId: scope.streamId,
						selection:
							'recordingId' in scope
								? { case: 'recordingId', value: scope.recordingId }
								: {
										case: 'range',
										value: { startMs: BigInt(scope.startMs), endMs: BigInt(scope.endMs) }
									}
					},
					reason
				}
			}
		});
		return this.request(command);
	}

	async confirm(
		job: RecordingDeletionJob,
		confirmationText: string
	): Promise<RecordingDeletionJob> {
		const confirmationNonce = this.#confirmations.get(job);
		if (!confirmationNonce) throw new Error('Preview this selection again before confirming.');
		if (confirmationText !== job.requiredConfirmationText)
			throw new Error('Confirmation text does not match.');
		this.#confirmations.delete(job);
		return this.request(
			create(RecordingMaintenanceCommandSchema, {
				action: {
					case: 'confirm',
					value: {
						jobId: job.jobId,
						expectedRevision: job.revision,
						confirmationNonce,
						confirmationText
					}
				}
			})
		);
	}

	get(jobId: string): Promise<RecordingDeletionJob> {
		return this.request(
			create(RecordingMaintenanceCommandSchema, { action: { case: 'get', value: { jobId } } })
		);
	}

	cancel(jobId: string): Promise<RecordingDeletionJob> {
		return this.request(
			create(RecordingMaintenanceCommandSchema, { action: { case: 'cancel', value: { jobId } } })
		);
	}

	retry(jobId: string): Promise<RecordingDeletionJob> {
		return this.request(
			create(RecordingMaintenanceCommandSchema, { action: { case: 'retry', value: { jobId } } })
		);
	}

	async list(afterJobId = ''): Promise<{ jobs: RecordingDeletionJob[]; nextJobId: string }> {
		const result = await this.send({
			case: 'recordingMaintenanceCommand',
			value: create(RecordingMaintenanceCommandSchema, {
				action: { case: 'list', value: { afterJobId } }
			})
		});
		if (result.case !== 'recordingDeletionJobs' || result.value.jobs.length > 16)
			throw new Error('Recording maintenance history is invalid.');
		return {
			jobs: result.value.jobs.map((job) => this.publicJob(job)),
			nextJobId: result.value.nextJobId
		};
	}

	inspectCatalog(): Promise<RecordingReconciliationReport> {
		return this.reconciliation(
			create(RecordingMaintenanceCommandSchema, { action: { case: 'inspectCatalog', value: {} } })
		);
	}

	applyRemedy(
		report: RecordingReconciliationReport,
		itemId: string,
		remedy: RecordingRemedy
	): Promise<RecordingReconciliationReport> {
		const item = report.items.find((item) => item.itemId === itemId);
		if (!report.complete || !item?.remedies.includes(remedy))
			return Promise.reject(new Error('Inspect a complete scope before applying this remedy.'));
		return this.reconciliation(
			create(RecordingMaintenanceCommandSchema, {
				action: { case: 'applyRemedy', value: { reportId: report.reportId, itemId, remedy } }
			})
		);
	}

	private async reconciliation(
		command: RecordingMaintenanceCommand
	): Promise<RecordingReconciliationReport> {
		const result = await this.send({ case: 'recordingMaintenanceCommand', value: command });
		if (result.case !== 'recordingReconciliationReport' || result.value.items.length > 128)
			throw new Error('Catalog reconciliation response is invalid.');
		return result.value;
	}

	private async request(command: RecordingMaintenanceCommand): Promise<RecordingDeletionJob> {
		const result = await this.send({ case: 'recordingMaintenanceCommand', value: command });
		if (result.case !== 'recordingDeletionJob')
			throw new Error('Recording maintenance response is invalid.');
		return this.publicJob(result.value);
	}

	private publicJob(value: RecordingDeletionJob): RecordingDeletionJob {
		if (value.objects.length > 128 || value.gaps.length > 128)
			throw new Error('Recording maintenance response exceeds limits.');
		const job = create(RecordingDeletionJobSchema, { ...value, confirmationNonce: undefined });
		if (value.confirmationNonce) this.#confirmations.set(job, value.confirmationNonce);
		return job;
	}
}
