import { fromJsonString } from '@bufbuild/protobuf';
import { RestoreRecordSchema, RestoreState, type RestoreRecord } from '$lib/proto/backup_pb';
import { ApiRequestError } from '$lib/api';

export const maximumConfigurationArchiveBytes = 1024 * 1024 * 1024;
const localErrorStatuses = new Set([400, 409, 410, 411, 413, 415, 422, 503]);

export class BackupHttpClient {
	constructor(private readonly accessKey: () => string | null) {}

	async apply(file: File, signal?: AbortSignal): Promise<RestoreRecord> {
		if (file.size === 0) throw new ApiRequestError(400, 'The configuration ZIP is empty.');
		if (file.size > maximumConfigurationArchiveBytes) {
			throw new ApiRequestError(413, 'The configuration ZIP exceeds the 1 GiB limit.');
		}
		const response = await this.request(
			'/config/apply',
			{
				method: 'POST',
				headers: this.headers({ 'Content-Type': 'application/zip', Accept: 'application/json' }),
				body: file
			},
			signal
		);
		const record = fromJsonString(RestoreRecordSchema, await response.text());
		if (record.state !== RestoreState.AWAITING_RESTART) {
			throw new ApiRequestError(502, 'Configuration apply did not return a staged restore.');
		}
		return record;
	}

	async export(signal?: AbortSignal): Promise<{ blob: Blob; fileName: string }> {
		const response = await this.request(
			'/config/export',
			{
				headers: this.headers({ Accept: 'application/zip' })
			},
			signal
		);
		if (response.headers.get('Content-Type')?.split(';')[0]?.trim() !== 'application/zip') {
			throw new ApiRequestError(502, 'Configuration export did not return a ZIP archive.');
		}
		if (Number(response.headers.get('Content-Length')) > maximumConfigurationArchiveBytes) {
			throw new ApiRequestError(413, 'The configuration ZIP exceeds the 1 GiB limit.');
		}
		const blob = await response.blob();
		if (blob.size === 0 || blob.size > maximumConfigurationArchiveBytes) {
			throw new ApiRequestError(502, 'The configuration ZIP has an invalid size.');
		}
		const disposition = response.headers.get('Content-Disposition') ?? '';
		const fileName = /filename="([^"/\\\r\n]+)"/.exec(disposition)?.[1] ?? 'keeppeek-config.zip';
		return { blob, fileName };
	}

	private async request(path: string, init: RequestInit, signal?: AbortSignal): Promise<Response> {
		const response = await fetch(path, {
			...init,
			cache: 'no-store',
			redirect: 'error',
			...(signal ? { signal } : {})
		});
		if (!response.ok) {
			let message = response.statusText;
			try {
				const error = (await response.json()) as { message?: unknown };
				if (typeof error.message === 'string') message = error.message;
			} catch {
				// The HTTP status remains authoritative when an intermediary returns non-JSON.
			}
			throw new ApiRequestError(response.status, message);
		}
		return response;
	}

	private headers(values: Record<string, string> = {}): Record<string, string> {
		const accessKey = this.accessKey();
		return accessKey ? { ...values, Authorization: `Bearer ${accessKey}` } : values;
	}
}

export function isLocalBackupError(error: unknown): boolean {
	return error instanceof ApiRequestError && localErrorStatuses.has(error.status);
}
