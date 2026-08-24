<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import StorageRetentionSection from '$lib/components/StorageRetentionSection.svelte';
	import type { SanitizedConfig, ServerHealthResponse } from '$lib/types';

	setCapabilityState(['keeppeek.runtime-config.v1']);

	const config = {
		host: '0.0.0.0',
		port: 3000,
		camera_count: 42,
		storage: {
			medium_term_path: '/mnt/keeppeek',
			long_term_path: '/mnt/keeppeek',
			recording_catalog_path: '/mnt/keeppeek/recordings.db',
			event_thumbnail_path: '/mnt/keeppeek/.event-thumbnails',
			event_thumbnail_max_mb: 1024,
			short_term_secs: 90,
			medium_term_secs: 1200,
			flush_interval_secs: 60,
			write_buffer_bytes: 8_192,
			long_term_max_gb: 2500
		},
		recording_estimate: {
			estimated_bitrate_bps: 318_000_000,
			bytes_per_day: 3_434_400_000_000,
			known_streams: 42,
			unknown_streams: 0,
			estimated_retention_days: 11
		}
	} satisfies SanitizedConfig;

	const health = {
		system: {
			disks: [
				{
					name: 'WD Red 8 TB',
					kind: 'SSD',
					file_system: 'ext4',
					mount_point: '/mnt/keeppeek',
					total_bytes: 8_000_000_000_000,
					available_bytes: 2_320_000_000_000,
					used_bytes: 5_680_000_000_000,
					removable: false,
					stores_recordings: true
				}
			]
		},
		storage: {
			long_term_max_bytes: 2_500_000_000_000,
			catalog_bytes: 8_000_000,
			catalog: {
				recording_files: 1_000,
				finalized_files: 990,
				active_files: 10,
				fragments: 50_000,
				fragment_bytes: 2_300_000_000_000,
				events: 1_402,
				open_events: 2,
				event_thumbnails: 350,
				oldest_recording_at_ms: Date.UTC(2026, 7, 13),
				newest_recording_at_ms: Date.UTC(2026, 7, 24)
			}
		}
	} as ServerHealthResponse;
</script>

<main
	data-paper-scenario="settings.desktop.storage-retention"
	class="h-[1163px] w-[1440px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<StorageRetentionSection {config} {health} onedit={() => {}} paperFrame />
</main>
