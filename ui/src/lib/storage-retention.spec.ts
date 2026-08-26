import { describe, expect, it } from 'vitest';
import {
	formatStorageBufferDuration,
	formatStorageDuration,
	suggestedStorageDisks,
	storageRetentionEvidence
} from '$lib/storage-retention';
import type { SanitizedConfig, ServerHealthResponse } from '$lib/types';

const config = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: 2,
	storage: {
		medium_term_path: '/recordings/active',
		long_term_path: '/recordings/archive',
		recording_catalog_path: '/recordings/recordings.db',
		event_thumbnail_path: '/recordings/events',
		event_thumbnail_max_mb: 1024,
		short_term_secs: 90,
		medium_term_secs: 1800,
		flush_interval_secs: 60,
		write_buffer_bytes: 1_048_576,
		long_term_max_gb: 2048
	},
	recording_estimate: {
		estimated_bitrate_bps: 8_000_000,
		bytes_per_day: 86_400_000_000,
		known_streams: 2,
		unknown_streams: 0,
		estimated_retention_days: 25.4
	}
} satisfies SanitizedConfig;

const health = {
	system: {
		disks: [
			{
				name: 'system',
				kind: 'ssd',
				file_system: 'apfs',
				mount_point: '/',
				total_bytes: 8_000_000_000_000,
				available_bytes: 2_000_000_000_000,
				used_bytes: 6_000_000_000_000,
				removable: false,
				stores_recordings: true
			},
			{
				name: 'recordings',
				kind: 'ssd',
				file_system: 'apfs',
				mount_point: '/recordings',
				total_bytes: 4_000_000_000_000,
				available_bytes: 1_500_000_000_000,
				used_bytes: 2_500_000_000_000,
				removable: false,
				stores_recordings: true
			}
		]
	},
	storage: {
		long_term_max_bytes: 2_199_023_255_552,
		catalog_bytes: 8_388_608,
		catalog: {
			fragment_bytes: 1_800_000_000_000,
			event_thumbnails: 350,
			oldest_recording_at_ms: 1_000,
			newest_recording_at_ms: 2_000
		}
	}
} as ServerHealthResponse;

describe('storage retention evidence', () => {
	it('formats zero and sub-second durations without rounding them to seconds', () => {
		expect(formatStorageDuration(0, 'en-US')).toBe('0 milliseconds');
		expect(formatStorageDuration(0.001, 'en-US')).toBe('1 millisecond');
		expect(formatStorageDuration(0.25, 'en-US')).toBe('250 milliseconds');
		expect(formatStorageDuration(1, 'en-US')).toBe('1 second');
		expect(formatStorageDuration(60, 'en-US')).toBe('1 minute');
		expect(formatStorageBufferDuration(0, 'en-US')).toBe('0 milliseconds');
		expect(formatStorageBufferDuration(90, 'en-US')).toBe('90 seconds');
	});

	it('keeps measured disk and catalog evidence distinct from projected retention', () => {
		const evidence = storageRetentionEvidence(config, health);

		expect(evidence).toMatchObject({
			recordingDisk: {
				mount_point: '/recordings',
				available_bytes: 1_500_000_000_000
			},
			indexedFragmentBytes: 1_800_000_000_000,
			catalogBytes: 8_388_608,
			eventThumbnailCount: 350,
			projectedRetentionDays: 25.4,
			oldestFootageAtMs: 1_000,
			newestFootageAtMs: 2_000,
			longTermCapBytes: 2_199_023_255_552,
			fillBehavior: 'prune-oldest',
			diskWarningThresholdPercent: 10
		});
	});

	it('describes runtime durations by their real storage-engine roles', () => {
		const evidence = storageRetentionEvidence(config, health);

		expect(evidence.shortTerm).toEqual({ durationSeconds: 90, storage: 'memory' });
		expect(evidence.activeWriter).toEqual({
			rolloverSeconds: 1800,
			flushSeconds: 60,
			writeBufferBytes: 1_048_576,
			path: '/recordings/active'
		});
		expect(evidence.archive).toEqual({
			path: '/recordings/archive',
			limitBytes: 2_199_023_255_552
		});
	});

	it('treats an explicit zero runtime archive limit as unlimited', () => {
		const unlimitedConfig = {
			...config,
			storage: { ...config.storage, long_term_max_gb: 0 }
		};
		const unlimitedHealth = {
			...health,
			storage: { ...health.storage, long_term_max_bytes: 0 }
		};
		const evidence = storageRetentionEvidence(unlimitedConfig, unlimitedHealth);

		expect(evidence.longTermCapBytes).toBeNull();
		expect(evidence.archive.limitBytes).toBeNull();
	});

	it('does not turn hysteresis into a limit when every trigger is disabled', () => {
		const unlimitedConfig = {
			...config,
			storage: {
				...config.storage,
				long_term_max_gb: 0,
				minimum_free_gb: 0,
				maximum_used_percent: null,
				warning_free_gb: 0,
				critical_free_gb: 0,
				cleanup_hysteresis_gb: 5
			}
		};
		const unlimitedHealth = {
			...health,
			storage: {
				...health.storage,
				long_term_max_bytes: 0,
				minimum_free_bytes: 0,
				maximum_used_percent: null,
				warning_free_bytes: 0,
				critical_free_bytes: 0,
				cleanup_hysteresis_bytes: 5 * 1_073_741_824
			}
		} as ServerHealthResponse;

		const evidence = storageRetentionEvidence(unlimitedConfig, unlimitedHealth);

		expect(evidence.warningFreeBytes).toBe(0);
		expect(evidence.recoveryFreeBytes).toBe(0);
		expect(evidence.effectiveLimitBytes).toBeNull();
		expect(evidence.pressure).toBe('normal');
	});

	it('combines archive, reserve, percentage, and non-KeepPeek usage into one effective limit', () => {
		const gibibyte = 1_073_741_824;
		const policyConfig = {
			...config,
			storage: {
				...config.storage,
				long_term_max_gb: 70,
				minimum_free_gb: 10,
				maximum_used_percent: 75,
				warning_free_gb: 20,
				critical_free_gb: 10,
				cleanup_hysteresis_gb: 5
			}
		};
		const policyHealth = {
			...health,
			system: {
				disks: [
					{
						...health.system.disks[1]!,
						total_bytes: 100 * gibibyte,
						available_bytes: 20 * gibibyte,
						used_bytes: 80 * gibibyte
					}
				]
			},
			storage: {
				...health.storage,
				long_term_max_bytes: 70 * gibibyte,
				catalog: { ...health.storage.catalog!, recording_bytes: 60 * gibibyte }
			}
		} as ServerHealthResponse;

		const evidence = storageRetentionEvidence(policyConfig, policyHealth);

		expect(evidence.keeppeekBytes).toBe(60 * gibibyte);
		expect(evidence.warningFreeBytes).toBe(25 * gibibyte);
		expect(evidence.recoveryFreeBytes).toBe(30 * gibibyte);
		expect(evidence.effectiveLimitBytes).toBe(55 * gibibyte);
	});

	it('does not synthesize disk, history, or override evidence without health', () => {
		const evidence = storageRetentionEvidence(config, null);

		expect(evidence).toMatchObject({
			recordingDisk: null,
			indexedFragmentBytes: null,
			catalogBytes: null,
			eventThumbnailCount: null,
			oldestFootageAtMs: null,
			newestFootageAtMs: null,
			perCameraOverrides: null,
			additionalLocations: null
		});
	});

	it('suggests one persistent row per mount and excludes tiny helper volumes', () => {
		const suggestions = suggestedStorageDisks(
			[
				...health.system.disks,
				{
					...health.system.disks[0]!,
					name: 'Macintosh HD duplicate',
					available_bytes: 1_000_000
				},
				{
					...health.system.disks[0]!,
					name: 'Paper helper',
					mount_point: '/Volumes/Paper',
					total_bytes: 8_000_000,
					available_bytes: 4_000_000,
					used_bytes: 4_000_000,
					stores_recordings: false
				},
				{
					...health.system.disks[0]!,
					name: 'Temporary overlay',
					file_system: 'tmpfs',
					mount_point: '/tmp/helper',
					stores_recordings: false
				}
			],
			'/recordings/archive'
		);

		expect(suggestions.map((disk) => disk.mount_point)).toEqual(['/recordings', '/']);
		expect(suggestions.filter((disk) => disk.mount_point === '/')).toHaveLength(1);
	});
});
