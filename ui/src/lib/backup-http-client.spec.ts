import { create, toJsonString } from '@bufbuild/protobuf';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { BackupHttpClient } from './backup-http-client';
import {
	BackupRecordSchema,
	BackupSection,
	BackupState,
	BackupTransferSchema,
	CreateBackupRequestSchema
} from './proto/backup_pb';

describe('BackupHttpClient', () => {
	afterEach(() => vi.restoreAllMocks());

	it('sends canonical ProtoJSON and parses the generated response', async () => {
		const record = create(BackupRecordSchema, {
			backupId: 'backup-1',
			state: BackupState.READY,
			archiveBytes: 1_048_576n
		});
		const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
			new Response(toJsonString(BackupRecordSchema, record), {
				status: 201,
				headers: { 'Content-Type': 'application/json' }
			})
		);
		const client = new BackupHttpClient(() => 'access-key');

		const created = await client.create(
			create(CreateBackupRequestSchema, {
				clientRequestId: 'request-1',
				sections: [BackupSection.RUNTIME_CONFIG],
				expectedArchiveBytes: 1_048_576n
			})
		);

		expect(created.archiveBytes).toBe(1_048_576n);
		const init = fetchMock.mock.calls[0]![1]!;
		expect(init.headers).toEqual({
			Authorization: 'Bearer access-key',
			'Content-Type': 'application/json'
		});
		expect(init.body).toContain('"expectedArchiveBytes":"1048576"');
	});

	it('uploads the File as application/zip after a ProtoJSON reservation', async () => {
		const transfer = create(BackupTransferSchema, {
			transferId: 'transfer-1',
			backupId: 'backup-1',
			uri: '/api/backups/transfers',
			contentType: 'application/zip',
			maximumBytes: 3n
		});
		const record = create(BackupRecordSchema, {
			backupId: 'backup-1',
			state: BackupState.READY
		});
		const fetchMock = vi
			.spyOn(globalThis, 'fetch')
			.mockResolvedValueOnce(new Response(toJsonString(BackupTransferSchema, transfer)))
			.mockResolvedValueOnce(new Response(toJsonString(BackupRecordSchema, record)));
		const client = new BackupHttpClient(() => null);
		const file = new File([new Uint8Array([1, 2, 3])], 'backup.zip');

		await client.upload(file);

		expect(fetchMock.mock.calls[1]![0]).toBe(
			'/api/backups/transfers?transfer_id=transfer-1'
		);
		expect(fetchMock.mock.calls[1]![1]).toMatchObject({
			method: 'PUT',
			body: file,
			headers: { 'Content-Type': 'application/zip' }
		});
	});
});