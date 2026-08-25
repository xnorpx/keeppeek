<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import FirstRunEmptyStates from '$lib/components/FirstRunEmptyStates.svelte';
	import FirstRunSetupPanel from '$lib/components/FirstRunSetupPanel.svelte';
	import type { SanitizedConfig } from '$lib/types';

	setCapabilityState();

	const config = {
		host: '0.0.0.0',
		port: 8080,
		storage: {
			medium_term_path: '/mnt/keeppeek',
			long_term_path: '/mnt/keeppeek',
			recording_catalog_path: '/mnt/keeppeek/recordings.db',
			event_thumbnail_path: '/mnt/keeppeek/.event-thumbnails',
			event_thumbnail_max_mb: 512,
			short_term_secs: 10,
			medium_term_secs: 3600,
			flush_interval_secs: 5,
			write_buffer_bytes: 1_048_576,
			long_term_max_gb: 0
		},
		camera_count: 0,
		recording_estimate: {
			estimated_bitrate_bps: 0,
			bytes_per_day: 0,
			known_streams: 0,
			unknown_streams: 0,
			estimated_retention_days: null
		}
	} satisfies SanitizedConfig;

	const health = {
		version: 'v0.4.1-pre',
		system: {
			disks: [
				{
					name: 'recordings',
					kind: 'SSD',
					file_system: 'apfs',
					mount_point: '/mnt/keeppeek',
					total_bytes: 8_000_000_000_000,
					available_bytes: 7_900_000_000_000,
					used_bytes: 100_000_000_000,
					removable: false,
					stores_recordings: true
				}
			]
		}
	};
</script>

<main
	data-paper-scenario="setup.desktop.first-run"
	class="flex h-[785px] w-[1440px] items-start gap-6 overflow-hidden bg-ground [font-synthesis:none]"
>
	<FirstRunSetupPanel
		{config}
		{health}
		timeZone="Europe/Stockholm"
		writeProbe={{ writable: true, detail: 'Write, flush, rename, and cleanup succeeded.' }}
		paperFrame
	/>
	<FirstRunEmptyStates cameraCount={config.camera_count} emptyDayLabel="18 AUG" paperFrame />
</main>
