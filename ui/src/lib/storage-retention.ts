import type { DiskHealth, SanitizedConfig, ServerHealthResponse } from '$lib/types';

const GIBIBYTE_BYTES = 1_073_741_824;

export type StorageRetentionEvidence = {
	recordingDisk: DiskHealth | null;
	indexedFragmentBytes: number | null;
	catalogBytes: number | null;
	eventThumbnailCount: number | null;
	projectedRetentionDays: number | null;
	oldestFootageAtMs: null;
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

	return {
		recordingDisk: health?.system.disks.find((disk) => disk.stores_recordings) ?? null,
		indexedFragmentBytes: health?.storage.catalog?.fragment_bytes ?? null,
		catalogBytes: health?.storage.catalog_bytes ?? null,
		eventThumbnailCount: health?.storage.catalog?.event_thumbnails ?? null,
		projectedRetentionDays: config.recording_estimate.estimated_retention_days,
		oldestFootageAtMs: null,
		longTermCapBytes: health?.storage.long_term_max_bytes ?? configuredCapBytes,
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
			limitBytes: health?.storage.long_term_max_bytes ?? configuredCapBytes
		},
		perCameraOverrides: null,
		additionalLocations: null
	};
}
