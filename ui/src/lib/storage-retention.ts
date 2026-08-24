import type { DiskHealth, SanitizedConfig, ServerHealthResponse } from '$lib/types';

const GIBIBYTE_BYTES = 1_073_741_824;

export function formatStorageDuration(seconds: number, locale?: string): string {
	if (seconds < 1) {
		const milliseconds = Math.max(0, Math.round(seconds * 1_000));
		return `${new Intl.NumberFormat(locale).format(milliseconds)} ${milliseconds === 1 ? 'millisecond' : 'milliseconds'}`;
	}
	if (seconds < 60) {
		const value = new Intl.NumberFormat(locale, { maximumFractionDigits: 2 }).format(seconds);
		return `${value} ${seconds === 1 ? 'second' : 'seconds'}`;
	}
	if (seconds % 3_600 === 0) {
		const hours = seconds / 3_600;
		return `${hours} ${hours === 1 ? 'hour' : 'hours'}`;
	}
	if (seconds % 60 === 0) {
		const minutes = seconds / 60;
		return `${minutes} ${minutes === 1 ? 'minute' : 'minutes'}`;
	}
	return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

export function formatStorageBufferDuration(seconds: number, locale?: string): string {
	if (seconds < 1) return formatStorageDuration(seconds, locale);
	const value = new Intl.NumberFormat(locale, { maximumFractionDigits: 2 }).format(seconds);
	return `${value} ${seconds === 1 ? 'second' : 'seconds'}`;
}

function normalizedPath(path: string): string {
	const normalized = path.trim().replaceAll('\\', '/');
	const withoutTrailingSlash = normalized.replace(/\/+$/, '');
	const result = withoutTrailingSlash || '/';
	return /^[A-Z]:($|\/)/i.test(result) ? result.toLowerCase() : result;
}

function pathIsWithinMount(path: string, mountPoint: string): boolean {
	const target = normalizedPath(path);
	const mount = normalizedPath(mountPoint);
	if (mount === '/') return target.startsWith('/');
	return target === mount || target.startsWith(`${mount}/`);
}

export function mostSpecificDiskForPath(
	path: string,
	disks: readonly DiskHealth[]
): DiskHealth | null {
	return (
		disks
			.filter((disk) => pathIsWithinMount(path, disk.mount_point))
			.toSorted(
				(left, right) =>
					normalizedPath(right.mount_point).length - normalizedPath(left.mount_point).length
			)
			.at(0) ?? null
	);
}

export type StorageRetentionEvidence = {
	recordingDisk: DiskHealth | null;
	indexedFragmentBytes: number | null;
	catalogBytes: number | null;
	eventThumbnailCount: number | null;
	projectedRetentionDays: number | null;
	oldestFootageAtMs: number | null;
	newestFootageAtMs: number | null;
	longTermCapBytes: number | null;
	fillBehavior: 'prune-oldest';
	diskWarningThresholdPercent: 10;
	shortTerm: {
		durationSeconds: number;
		storage: 'memory';
	};
	activeWriter: {
		rolloverSeconds: number;
		flushSeconds: number;
		writeBufferBytes: number;
		path: string;
	};
	archive: {
		path: string;
		limitBytes: number | null;
	};
	perCameraOverrides: null;
	additionalLocations: null;
};

export function storageRetentionEvidence(
	config: SanitizedConfig,
	health: ServerHealthResponse | null
): StorageRetentionEvidence {
	const configuredCapBytes =
		config.storage.long_term_max_gb === 0 ? null : config.storage.long_term_max_gb * GIBIBYTE_BYTES;
	const runtimeCapBytes = health?.storage.long_term_max_bytes;
	const effectiveCapBytes =
		runtimeCapBytes === undefined || runtimeCapBytes === null
			? configuredCapBytes
			: runtimeCapBytes === 0
				? null
				: runtimeCapBytes;

	return {
		recordingDisk: mostSpecificDiskForPath(
			config.storage.long_term_path,
			health?.system.disks ?? []
		),
		indexedFragmentBytes: health?.storage.catalog?.fragment_bytes ?? null,
		catalogBytes: health?.storage.catalog_bytes ?? null,
		eventThumbnailCount: health?.storage.catalog?.event_thumbnails ?? null,
		projectedRetentionDays: config.recording_estimate.estimated_retention_days,
		oldestFootageAtMs: health?.storage.catalog?.oldest_recording_at_ms ?? null,
		newestFootageAtMs: health?.storage.catalog?.newest_recording_at_ms ?? null,
		longTermCapBytes: effectiveCapBytes,
		fillBehavior: 'prune-oldest',
		diskWarningThresholdPercent: 10,
		shortTerm: {
			durationSeconds: config.storage.short_term_secs,
			storage: 'memory'
		},
		activeWriter: {
			rolloverSeconds: config.storage.medium_term_secs,
			flushSeconds: config.storage.flush_interval_secs,
			writeBufferBytes: config.storage.write_buffer_bytes,
			path: config.storage.medium_term_path
		},
		archive: {
			path: config.storage.long_term_path,
			limitBytes: effectiveCapBytes
		},
		perCameraOverrides: null,
		additionalLocations: null
	};
}
