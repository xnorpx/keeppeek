import type { CatalogHealth, DiskHealth, SanitizedConfig, ServerHealthResponse } from '$lib/types';

const GIBIBYTE_BYTES = 1_073_741_824;
const MINIMUM_SUGGESTED_STORAGE_BYTES = 16 * GIBIBYTE_BYTES;
const NON_PERSISTENT_FILE_SYSTEMS = new Set([
	'autofs',
	'devfs',
	'overlay',
	'proc',
	'squashfs',
	'sysfs',
	'tmpfs'
]);

export type StoragePolicyInput = {
	archiveMaxBytes: number;
	minimumFreeBytes: number;
	maximumUsedPercent: number | null;
	warningFreeBytes: number;
	criticalFreeBytes: number;
	cleanupHysteresisBytes: number;
	capacity: Pick<DiskHealth, 'total_bytes' | 'available_bytes'> | null;
	keeppeekBytes: number;
};

export type EffectiveStoragePolicy = {
	archiveLimitBytes: number | null;
	warningFreeBytes: number;
	criticalFreeBytes: number;
	recoveryFreeBytes: number;
	effectiveLimitBytes: number | null;
	cleanupTargetBytes: number | null;
	pressure: 'normal' | 'warning' | 'critical';
};

export function effectiveStoragePolicy(input: StoragePolicyInput): EffectiveStoragePolicy {
	const archiveLimitBytes = input.archiveMaxBytes === 0 ? null : input.archiveMaxBytes;
	const criticalFreeBytes = Math.max(input.minimumFreeBytes, input.criticalFreeBytes);
	const configuredWarningFreeBytes =
		input.warningFreeBytes === 0 && criticalFreeBytes > 0
			? criticalFreeBytes + input.cleanupHysteresisBytes
			: Math.max(input.warningFreeBytes, criticalFreeBytes);
	const percentageFreeBytes =
		input.capacity && input.maximumUsedPercent !== null
			? input.capacity.total_bytes -
				Math.floor((input.capacity.total_bytes * Math.min(100, input.maximumUsedPercent)) / 100)
			: 0;
	const warningFreeBytes = Math.max(configuredWarningFreeBytes, percentageFreeBytes);
	const hasFilesystemLimit =
		warningFreeBytes > 0 ||
		input.minimumFreeBytes > 0 ||
		input.maximumUsedPercent !== null ||
		criticalFreeBytes > 0;
	const recoveryFreeBytes = hasFilesystemLimit
		? Math.min(
				input.capacity?.total_bytes ?? Number.MAX_SAFE_INTEGER,
				warningFreeBytes + input.cleanupHysteresisBytes
			)
		: 0;
	const otherUsedBytes = input.capacity
		? Math.max(0, input.capacity.total_bytes - input.capacity.available_bytes - input.keeppeekBytes)
		: 0;
	const filesystemLimitBytes =
		input.capacity && hasFilesystemLimit
			? Math.max(0, input.capacity.total_bytes - otherUsedBytes - warningFreeBytes)
			: null;
	const effectiveLimitBytes = [archiveLimitBytes, filesystemLimitBytes]
		.filter((value): value is number => value !== null)
		.reduce<number | null>(
			(limit, value) => (limit === null ? value : Math.min(limit, value)),
			null
		);
	const archiveTargetBytes =
		archiveLimitBytes === null
			? null
			: Math.max(0, archiveLimitBytes - input.cleanupHysteresisBytes);
	const filesystemTargetBytes =
		input.capacity && hasFilesystemLimit
			? Math.max(0, input.capacity.total_bytes - otherUsedBytes - recoveryFreeBytes)
			: null;
	const cleanupRequired =
		(archiveLimitBytes !== null && input.keeppeekBytes > archiveLimitBytes) ||
		(input.capacity !== null &&
			hasFilesystemLimit &&
			input.capacity.available_bytes < warningFreeBytes);
	const cleanupTargetBytes = cleanupRequired
		? [archiveTargetBytes, filesystemTargetBytes]
				.filter((value): value is number => value !== null)
				.reduce<number | null>(
					(target, value) => (target === null ? value : Math.min(target, value)),
					null
				)
		: null;
	const pressure =
		input.capacity && criticalFreeBytes > 0 && input.capacity.available_bytes < criticalFreeBytes
			? 'critical'
			: cleanupRequired
				? 'warning'
				: 'normal';

	return {
		archiveLimitBytes,
		warningFreeBytes,
		criticalFreeBytes,
		recoveryFreeBytes,
		effectiveLimitBytes,
		cleanupTargetBytes,
		pressure
	};
}

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

export function suggestedStorageDisks(
	disks: readonly DiskHealth[],
	currentPath: string
): DiskHealth[] {
	const current = mostSpecificDiskForPath(currentPath, disks);
	const byMount = new Map<string, DiskHealth>();
	for (const disk of disks) {
		const key = normalizedPath(disk.mount_point);
		const existing = byMount.get(key);
		if (
			!existing ||
			disk.stores_recordings ||
			(!existing.stores_recordings && disk.available_bytes > existing.available_bytes)
		) {
			byMount.set(key, disk);
		}
	}
	return [...byMount.values()]
		.filter(
			(disk) =>
				disk.mount_point === current?.mount_point ||
				(disk.total_bytes >= MINIMUM_SUGGESTED_STORAGE_BYTES &&
					disk.available_bytes > 0 &&
					!NON_PERSISTENT_FILE_SYSTEMS.has(disk.file_system.toLocaleLowerCase()))
		)
		.toSorted((left, right) => {
			if (left.mount_point === current?.mount_point) return -1;
			if (right.mount_point === current?.mount_point) return 1;
			return right.available_bytes - left.available_bytes;
		});
}

export function catalogRecordingBytes(catalog: CatalogHealth | null | undefined): number | null {
	if (!catalog) return null;
	const exact = catalog.recording_bytes;
	if (exact !== undefined && (exact > 0 || catalog.fragment_bytes === 0)) return exact;
	return catalog.fragment_bytes;
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
	keeppeekBytes: number | null;
	minimumFreeBytes: number;
	maximumUsedPercent: number | null;
	warningFreeBytes: number;
	criticalFreeBytes: number;
	recoveryFreeBytes: number;
	effectiveLimitBytes: number | null;
	cleanupTargetBytes: number | null;
	pressure: 'normal' | 'warning' | 'critical';
	recordingState: 'active' | 'degraded' | 'paused';
	cleanupRunning: boolean;
	lastCleanupFilesRemoved: number;
	lastCleanupBytesRemoved: number;
	lastCleanupReason: string | null;
	lastCleanupEndedAtMs: number | null;
	lastFailure: string | null;
	fillBehavior: 'prune-oldest';
	diskWarningThresholdPercent: number;
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
	const recordingDisk = mostSpecificDiskForPath(
		config.storage.long_term_path,
		health?.system.disks ?? []
	);
	const safety = health?.storage.safety;
	const hasPolicyEvidence =
		safety !== undefined ||
		health?.storage.minimum_free_bytes !== undefined ||
		config.storage.minimum_free_gb !== undefined;
	const keeppeekBytes = safety?.keeppeek_bytes ?? catalogRecordingBytes(health?.storage.catalog);
	const minimumFreeBytes =
		health?.storage.minimum_free_bytes ?? (config.storage.minimum_free_gb ?? 0) * GIBIBYTE_BYTES;
	const maximumUsedPercent =
		health?.storage.maximum_used_percent ?? config.storage.maximum_used_percent ?? null;
	const configuredWarningFreeBytes =
		health?.storage.warning_free_bytes ?? (config.storage.warning_free_gb ?? 0) * GIBIBYTE_BYTES;
	const criticalFreeBytes = Math.max(
		minimumFreeBytes,
		health?.storage.critical_free_bytes ?? (config.storage.critical_free_gb ?? 0) * GIBIBYTE_BYTES
	);
	const warningFallback =
		configuredWarningFreeBytes === 0 && criticalFreeBytes > 0
			? criticalFreeBytes +
				(health?.storage.cleanup_hysteresis_bytes ??
					(config.storage.cleanup_hysteresis_gb ?? 0) * GIBIBYTE_BYTES)
			: configuredWarningFreeBytes;
	const cleanupHysteresisBytes =
		health?.storage.cleanup_hysteresis_bytes ??
		(config.storage.cleanup_hysteresis_gb ?? 0) * GIBIBYTE_BYTES;
	const computedPolicy = effectiveStoragePolicy({
		archiveMaxBytes: effectiveCapBytes ?? 0,
		minimumFreeBytes,
		maximumUsedPercent,
		warningFreeBytes: warningFallback,
		criticalFreeBytes,
		cleanupHysteresisBytes,
		capacity: recordingDisk,
		keeppeekBytes: keeppeekBytes ?? 0
	});
	const warningFreeBytes = safety?.warning_free_bytes ?? computedPolicy.warningFreeBytes;
	const recoveryFreeBytes = safety?.recovery_free_bytes ?? computedPolicy.recoveryFreeBytes;
	const effectiveLimitBytes = safety?.effective_limit_bytes ?? computedPolicy.effectiveLimitBytes;
	const pressure = safety?.pressure ?? computedPolicy.pressure;

	return {
		recordingDisk,
		indexedFragmentBytes: health?.storage.catalog?.fragment_bytes ?? null,
		catalogBytes: health?.storage.catalog_bytes ?? null,
		eventThumbnailCount: health?.storage.catalog?.event_thumbnails ?? null,
		projectedRetentionDays: config.recording_estimate.estimated_retention_days,
		oldestFootageAtMs: health?.storage.catalog?.oldest_recording_at_ms ?? null,
		newestFootageAtMs: health?.storage.catalog?.newest_recording_at_ms ?? null,
		longTermCapBytes: effectiveCapBytes,
		keeppeekBytes,
		minimumFreeBytes,
		maximumUsedPercent,
		warningFreeBytes,
		criticalFreeBytes,
		recoveryFreeBytes,
		effectiveLimitBytes,
		cleanupTargetBytes: safety?.cleanup_target_bytes ?? computedPolicy.cleanupTargetBytes,
		pressure,
		recordingState: safety?.recording_state ?? (pressure === 'normal' ? 'active' : 'degraded'),
		cleanupRunning: safety?.cleanup_running ?? false,
		lastCleanupFilesRemoved: safety?.last_cleanup_files_removed ?? 0,
		lastCleanupBytesRemoved: safety?.last_cleanup_bytes_removed ?? 0,
		lastCleanupReason: safety?.last_cleanup_reason ?? null,
		lastCleanupEndedAtMs: safety?.last_cleanup_ended_at_ms ?? null,
		lastFailure: safety?.last_failure ?? null,
		fillBehavior: 'prune-oldest',
		diskWarningThresholdPercent:
			hasPolicyEvidence && recordingDisk && recordingDisk.total_bytes > 0
				? (warningFreeBytes / recordingDisk.total_bytes) * 100
				: 10,
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
