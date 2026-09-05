import { create, toJsonString } from '@bufbuild/protobuf';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { BackupHttpClient } from './backup-http-client';
import { RestoreRecordSchema, RestoreState } from './proto/backup_pb';

describe('BackupHttpClient', () => {
	afterEach(() => vi.restoreAllMocks());

	it('downloads a fresh ZIP directly from config/export', async () => {
		const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
			new Response('configuration archive', {
				headers: {
					'Content-Type': 'application/zip',
					'Content-Disposition': 'attachment; filename="keeppeek-config.zip"'
				}
			})
		);
		const client = new BackupHttpClient(() => 'access-key');

		const exported = await client.export();

		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(fetchMock).toHaveBeenCalledWith('/config/export', {
			headers: { Authorization: 'Bearer access-key', Accept: 'application/zip' },
			cache: 'no-store',
			redirect: 'error'
		});
		expect(exported.fileName).toBe('keeppeek-config.zip');
		expect(await exported.blob.text()).toBe('configuration archive');
	});

	it('posts the ZIP directly to config/apply and parses the staged response', async () => {
		const record = create(RestoreRecordSchema, {
			restoreId: 'restore-1',
			state: RestoreState.AWAITING_RESTART
		});
		const fetchMock = vi
			.spyOn(globalThis, 'fetch')
			.mockResolvedValue(new Response(toJsonString(RestoreRecordSchema, record), { status: 202 }));
		const client = new BackupHttpClient(() => null);
		const file = new File([new Uint8Array([1, 2, 3])], 'backup.zip');

		const applied = await client.apply(file);

		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(fetchMock).toHaveBeenCalledWith('/config/apply', {
			method: 'POST',
			body: file,
			headers: { 'Content-Type': 'application/zip', Accept: 'application/json' },
			cache: 'no-store',
			redirect: 'error'
		});
		expect(applied.state).toBe(RestoreState.AWAITING_RESTART);
	});

	it('rejects an HTML response instead of downloading it as a ZIP', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValue(
			new Response('<html>not a ZIP</html>', { headers: { 'Content-Type': 'text/html' } })
		);
		await expect(new BackupHttpClient(() => null).export()).rejects.toThrow('ZIP');
	});

	it('preserves typed apply errors for the local configuration editor', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValue(
			new Response(JSON.stringify({ message: 'configuration archive failed validation' }), {
				status: 400
			})
		);
		await expect(
			new BackupHttpClient(() => null).apply(new File(['invalid'], 'configuration.zip'))
		).rejects.toMatchObject({ status: 400, message: 'configuration archive failed validation' });
	});

	it('refuses an empty archive before making a request', async () => {
		const fetchMock = vi.spyOn(globalThis, 'fetch');
		await expect(
			new BackupHttpClient(() => null).apply(new File([], 'configuration.zip'))
		).rejects.toMatchObject({ status: 400 });
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
