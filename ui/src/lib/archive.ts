import { create } from '@bufbuild/protobuf';
import type { ControlClient } from './control-client';
import {
	ArchiveCommandSchema,
	ArchiveSettingsSchema,
	type ArchiveSettings,
	type StorageObject
} from './proto/webrtc_pb';

export type ArchiveController = Pick<
	ControlClient,
	'archive' | 'storageVolumes' | 'updateRuntimeConfiguration' | 'getRuntimeConfiguration'
>;
export const archiveNumberFields = [
	{ key: 'minimumAgeSecs', label: 'Minimum age seconds', min: 0n, max: 315_360_000n },
	{ key: 'maximumObjectBytes', label: 'Maximum object bytes', min: 1n, max: 68_719_476_736n },
	{ key: 'maximumPendingCount', label: 'Maximum pending jobs', min: 1n, max: 4096n },
	{
		key: 'maximumPendingBytes',
		label: 'Maximum pending bytes',
		min: 1n,
		max: 9_223_372_036_854_775_807n
	},
	{ key: 'operationTimeoutSecs', label: 'Operation timeout seconds', min: 1n, max: 3600n },
	{ key: 'maximumAttempts', label: 'Maximum attempts', min: 1n, max: 10n }
] as const;
type NumberKey = (typeof archiveNumberFields)[number]['key'];
export type ArchiveDraft = Record<NumberKey, string> & {
	targetId: string;
	root: string;
	enabled: boolean;
	automatic: boolean;
	evictAfterArchive: boolean;
	sourceIds: string;
};

export function archiveDraft(value?: ArchiveSettings): ArchiveDraft {
	const settings =
		value ??
		create(ArchiveSettingsSchema, {
			targetId: 'local-archive',
			maximumObjectBytes: 4_294_967_296n,
			maximumPendingCount: 256,
			maximumPendingBytes: 68_719_476_736n,
			operationTimeoutSecs: 120n,
			maximumAttempts: 3
		});
	return {
		targetId: settings.targetId,
		root: settings.root,
		enabled: settings.enabled,
		automatic: settings.automatic,
		evictAfterArchive: settings.evictAfterArchive,
		sourceIds: settings.sourceIds.join('\n'),
		minimumAgeSecs: settings.minimumAgeSecs.toString(),
		maximumObjectBytes: settings.maximumObjectBytes.toString(),
		maximumPendingCount: settings.maximumPendingCount.toString(),
		maximumPendingBytes: settings.maximumPendingBytes.toString(),
		operationTimeoutSecs: settings.operationTimeoutSecs.toString(),
		maximumAttempts: settings.maximumAttempts.toString()
	};
}

export function encodeArchiveDraft(draft: ArchiveDraft): ArchiveSettings {
	if (!draft.targetId.trim() || !draft.root.trim())
		throw new Error('Target ID and archive root are required.');
	const values = {} as Record<NumberKey, bigint>;
	for (const field of archiveNumberFields) {
		const text = draft[field.key];
		if (!/^\d{1,19}$/.test(text)) throw new Error(`${field.label} must be a whole number.`);
		const value = BigInt(text);
		if (value < field.min || value > field.max)
			throw new Error(`${field.label} must be between ${field.min} and ${field.max}.`);
		values[field.key] = value;
	}
	if (values.maximumObjectBytes > values.maximumPendingBytes)
		throw new Error('Maximum object bytes must fit the pending byte budget.');
	const sourceIds =
		draft.sourceIds === '' ? [] : draft.sourceIds.split('\n').map((value) => value.trim());
	if (
		sourceIds.length > 127 ||
		sourceIds.some(
			(value) => !value || new TextEncoder().encode(value).length > 256 || /\p{Cc}/u.test(value)
		)
	)
		throw new Error('Use at most 127 nonempty source IDs of at most 256 bytes.');
	if (new Set(sourceIds).size !== sourceIds.length) throw new Error('Source IDs must be unique.');
	return create(ArchiveSettingsSchema, {
		...values,
		maximumPendingCount: Number(values.maximumPendingCount),
		maximumAttempts: Number(values.maximumAttempts),
		targetId: draft.targetId,
		root: draft.root,
		enabled: draft.enabled,
		automatic: draft.automatic,
		evictAfterArchive: draft.evictAfterArchive,
		sourceIds
	});
}

export function archiveDraftError(draft: ArchiveDraft): string | null {
	try {
		encodeArchiveDraft(draft);
		return null;
	} catch (cause) {
		return cause instanceof Error ? cause.message : 'Archive settings are invalid.';
	}
}

export function archiveJobId(
	random: {
		randomUUID?: () => string;
		getRandomValues?: (bytes: Uint8Array<ArrayBuffer>) => void;
	} = globalThis.crypto
): string {
	if (typeof random?.randomUUID === 'function') return random.randomUUID();
	if (typeof random?.getRandomValues !== 'function')
		throw new Error('Secure random IDs are unavailable. Use HTTPS or localhost, then retry.');
	const bytes = new Uint8Array(16);
	random.getRandomValues(bytes);
	bytes[6] = (bytes[6]! & 0x0f) | 0x40;
	bytes[8] = (bytes[8]! & 0x3f) | 0x80;
	const hex = Array.from(bytes, (value) => value.toString(16).padStart(2, '0')).join('');
	return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export async function submitArchiveCopy(
	controller: Pick<ArchiveController, 'archive'>,
	object: Pick<StorageObject, 'id' | 'kind'>,
	jobId: string
) {
	try {
		const result = await controller.archive(
			create(ArchiveCommandSchema, {
				action: { case: 'copy', value: { jobId, objectId: object.id, kind: object.kind } }
			})
		);
		if (result.result.case !== 'job') throw new Error('Unexpected archive copy response.');
		return result.result.value;
	} catch (cause) {
		// ponytail: reconcile the same durable intent; do not invent a second copy protocol.
		const result = await controller
			.archive(create(ArchiveCommandSchema, { action: { case: 'get', value: { jobId } } }))
			.catch(() => null);
		if (result?.result.case === 'job') return result.result.value;
		throw cause;
	}
}
