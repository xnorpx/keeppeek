import type { DiskHealth } from '$lib/types';
import { mostSpecificDiskForPath } from '$lib/storage-retention';

export type StorageWriteProbe = {
	writable: boolean;
	detail: string;
};

export type FirstRunStorageEvidence = {
	path: string;
	diskName: string | null;
	mountPoint: string | null;
	availableBytes: number | null;
	writeStatus: 'verified' | 'failed' | 'unavailable';
	detail: string;
	canStartRecorder: boolean;
};

export function firstRunStorageEvidence(
	path: string,
	disks: readonly DiskHealth[],
	writeProbe: StorageWriteProbe | null
): FirstRunStorageEvidence {
	const disk = mostSpecificDiskForPath(path, disks);
	const writeStatus =
		writeProbe === null ? 'unavailable' : writeProbe.writable ? 'verified' : 'failed';

	return {
		path,
		diskName: disk?.name ?? null,
		mountPoint: disk?.mount_point ?? null,
		availableBytes: disk?.available_bytes ?? null,
		writeStatus,
		detail: writeProbe?.detail ?? 'Storage write verification has not run yet.',
		canStartRecorder: writeProbe?.writable === true
	};
}

export function detectedBrowserTimeZone(): string | null {
	return Intl.DateTimeFormat().resolvedOptions().timeZone || null;
}
