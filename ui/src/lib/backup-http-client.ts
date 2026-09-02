import {
	create,
	fromJsonString,
	toJsonString,
	type DescMessage,
	type MessageShape
} from '@bufbuild/protobuf';
import {
	ActivateRestoreRequestSchema,
	BackupCapabilitiesSchema,
	BackupRecordSchema,
	BackupTransferSchema,
	CreateBackupRequestSchema,
	CreateRestorePlanRequestSchema,
	DeleteBackupRequestSchema,
	DeleteBackupResponseSchema,
	GetRestoreRequestSchema,
	InspectBackupRequestSchema,
	ListBackupsResponseSchema,
	RestorePlanSchema,
	RestoreRecordSchema,
	RollbackRestoreRequestSchema,
	BeginBackupUploadRequestSchema,
	type ActivateRestoreRequest,
	type BackupCapabilities,
	type BackupRecord,
	type BackupTransfer,
	type CreateBackupRequest,
	type CreateRestorePlanRequest,
	type DeleteBackupRequest,
	type DeleteBackupResponse,
	type ListBackupsResponse,
	type RestorePlan,
	type RestoreRecord,
	type RollbackRestoreRequest
} from '$lib/proto/backup_pb';
import { ApiRequestError } from '$lib/api';

const localErrorStatuses = new Set([400, 409, 410, 411, 413, 415, 422, 503]);

export class BackupHttpClient {
	constructor(private readonly accessKey: () => string | null) {}

	capabilities(): Promise<BackupCapabilities> {
		return this.get('/api/backups/capabilities', BackupCapabilitiesSchema);
	}

	list(): Promise<ListBackupsResponse> {
		return this.get('/api/backups', ListBackupsResponseSchema);
	}

	create(request: CreateBackupRequest): Promise<BackupRecord> {
		return this.post('/api/backups', CreateBackupRequestSchema, request, BackupRecordSchema);
	}

	async upload(file: File): Promise<BackupRecord> {
		const transfer = await this.post(
			'/api/backups/uploads',
			BeginBackupUploadRequestSchema,
			create(BeginBackupUploadRequestSchema, {
				clientRequestId: crypto.randomUUID(),
				fileName: file.name,
				contentLength: BigInt(file.size)
			}),
			BackupTransferSchema
		);
		return this.request(
			`${transfer.uri}?transfer_id=${encodeURIComponent(transfer.transferId)}`,
			{
				method: 'PUT',
				headers: this.headers({ 'Content-Type': 'application/zip' }),
				body: file
			},
			BackupRecordSchema
		);
	}

	createRestorePlan(request: CreateRestorePlanRequest): Promise<RestorePlan> {
		return this.post(
			'/api/backups/restore-plans',
			CreateRestorePlanRequestSchema,
			request,
			RestorePlanSchema
		);
	}

	inspect(backupId: string): Promise<BackupRecord> {
		return this.post(
			'/api/backups/inspect',
			InspectBackupRequestSchema,
			create(InspectBackupRequestSchema, { backupId }),
			BackupRecordSchema
		);
	}

	activate(request: ActivateRestoreRequest): Promise<RestoreRecord> {
		return this.post(
			'/api/backups/restores',
			ActivateRestoreRequestSchema,
			request,
			RestoreRecordSchema
		);
	}

	getRestore(restoreId: string): Promise<RestoreRecord> {
		return this.post(
			'/api/backups/restores/get',
			GetRestoreRequestSchema,
			create(GetRestoreRequestSchema, { restoreId }),
			RestoreRecordSchema
		);
	}

	rollback(request: RollbackRestoreRequest): Promise<RestoreRecord> {
		return this.post(
			'/api/backups/rollbacks',
			RollbackRestoreRequestSchema,
			request,
			RestoreRecordSchema
		);
	}

	delete(request: DeleteBackupRequest): Promise<DeleteBackupResponse> {
		return this.post(
			'/api/backups/delete',
			DeleteBackupRequestSchema,
			request,
			DeleteBackupResponseSchema
		);
	}

	downloadUrl(backupId: string): string {
		return `/api/backups/download?backup_id=${encodeURIComponent(backupId)}`;
	}

	async download(backupId: string): Promise<{ blob: Blob; fileName: string }> {
		const response = await fetch(this.downloadUrl(backupId), {
			headers: this.headers({ Accept: 'application/zip' }),
			cache: 'no-store'
		});
		if (!response.ok) throw new ApiRequestError(response.status, response.statusText);
		const disposition = response.headers.get('Content-Disposition') ?? '';
		const fileName = /filename="([^"]+)"/.exec(disposition)?.[1] ?? `keeppeek-${backupId}.zip`;
		return { blob: await response.blob(), fileName };
	}

	private get<Desc extends DescMessage>(path: string, schema: Desc): Promise<MessageShape<Desc>> {
		return this.request(path, { headers: this.headers() }, schema);
	}

	private post<Input extends DescMessage, Output extends DescMessage>(
		path: string,
		inputSchema: Input,
		input: MessageShape<Input>,
		outputSchema: Output
	): Promise<MessageShape<Output>> {
		return this.request(
			path,
			{
				method: 'POST',
				headers: this.headers({ 'Content-Type': 'application/json' }),
				body: toJsonString(inputSchema, input)
			},
			outputSchema
		);
	}

	private async request<Desc extends DescMessage>(
		path: string,
		init: RequestInit,
		schema: Desc
	): Promise<MessageShape<Desc>> {
		const response = await fetch(path, { ...init, cache: 'no-store' });
		const text = await response.text();
		if (!response.ok) {
			let message = response.statusText;
			try {
				const error = JSON.parse(text) as { message?: unknown };
				if (typeof error.message === 'string') message = error.message;
			} catch {
				// The HTTP status remains authoritative when an intermediary returns non-JSON.
			}
			throw new ApiRequestError(response.status, message);
		}
		return fromJsonString(schema, text);
	}

	private headers(values: Record<string, string> = {}): Record<string, string> {
		const accessKey = this.accessKey();
		return accessKey ? { ...values, Authorization: `Bearer ${accessKey}` } : values;
	}
}

export function isLocalBackupError(error: unknown): boolean {
	return error instanceof ApiRequestError && localErrorStatuses.has(error.status);
}
