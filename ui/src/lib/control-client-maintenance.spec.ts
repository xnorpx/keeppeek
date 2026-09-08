import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import { RecordingMaintenanceClient } from './control-client-maintenance';
import {
	RecordingDeletionJobSchema,
	RecordingDeletionReason,
	RecordingDeletionStatus,
	type Request
} from './proto/webrtc_pb';

describe('recording maintenance client', () => {
	it('keeps the confirmation nonce out of job objects and binds it to the preview', async () => {
		const requests: Request['command'][] = [];
		const client = new RecordingMaintenanceClient(async (command) => {
			requests.push(command);
			return {
				case: 'recordingDeletionJob',
				value: create(RecordingDeletionJobSchema, {
					jobId: '1'.repeat(32),
					revision: 12n,
					confirmationNonce: '2'.repeat(64),
					status: RecordingDeletionStatus.PREPARED,
					requiredConfirmationText: 'DELETE 1'
				})
			};
		});
		const job = await client.preview(
			{ sourceId: 'front', streamId: 'sub', startMs: 1_000, endMs: 2_000 },
			RecordingDeletionReason.OPERATOR
		);
		expect(job.confirmationNonce).toBeUndefined();
		await client.confirm(job, 'DELETE 1');
		const command = requests[1];
		expect(command?.case).toBe('recordingMaintenanceCommand');
		if (command?.case !== 'recordingMaintenanceCommand' || command.value.action.case !== 'confirm')
			throw new Error('confirmation command missing');
		expect(command.value.action.value.confirmationNonce).toBe('2'.repeat(64));
		expect(command.value.action.value.expectedRevision).toBe(12n);
		await expect(
			client.confirm(create(RecordingDeletionJobSchema, { jobId: job.jobId }), 'DELETE 1')
		).rejects.toThrow('Preview');
	});

	it('rejects invalid or oversized ranges before sending a request', async () => {
		let calls = 0;
		const client = new RecordingMaintenanceClient(async () => {
			calls += 1;
			return { case: undefined };
		});
		for (const endMs of [0, Number.NaN, 32 * 86_400_000]) {
			await expect(
				client.preview(
					{ sourceId: 'front', streamId: 'sub', startMs: 0, endMs },
					RecordingDeletionReason.OPERATOR
				)
			).rejects.toThrow();
		}
		expect(calls).toBe(0);
	});

	it('requires a fresh preview after a failed confirmation attempt', async () => {
		let attempts = 0;
		const client = new RecordingMaintenanceClient(async (command) => {
			if (
				command.case === 'recordingMaintenanceCommand' &&
				command.value.action.case === 'confirm'
			) {
				attempts += 1;
				throw new Error('Catalog changed; preview again.');
			}
			return {
				case: 'recordingDeletionJob',
				value: create(RecordingDeletionJobSchema, {
					jobId: '1'.repeat(32),
					revision: 1n,
					confirmationNonce: '2'.repeat(64),
					status: RecordingDeletionStatus.PREPARED,
					requiredConfirmationText: 'DELETE 1'
				})
			};
		});
		const preview = await client.preview(
			{ sourceId: 'front', streamId: 'sub', startMs: 1_000, endMs: 2_000 },
			RecordingDeletionReason.OPERATOR
		);
		await expect(client.confirm(preview, 'DELETE 1')).rejects.toThrow('Catalog changed');
		await expect(client.confirm(preview, 'DELETE 1')).rejects.toThrow(
			'Preview this selection again'
		);
		expect(attempts).toBe(1);
	});
});
