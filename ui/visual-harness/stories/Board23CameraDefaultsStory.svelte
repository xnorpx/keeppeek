<script lang="ts">
	import CameraDefaultsSection from '$lib/components/CameraDefaultsSection.svelte';
	import type { CameraSettings, SanitizedConfig } from '$lib/types';

	const config = {
		host: '0.0.0.0',
		port: 3000,
		camera_count: 42,
		storage: {
			medium_term_path: '/recordings/medium',
			long_term_path: '/recordings/long',
			recording_catalog_path: '/recordings/long/recordings.db',
			event_thumbnail_path: '/recordings/long/.event-thumbnails',
			event_thumbnail_max_mb: 1024,
			short_term_secs: 120,
			medium_term_secs: 1800,
			flush_interval_secs: 60,
			write_buffer_bytes: 8192,
			long_term_max_gb: 2048
		},
		recording_estimate: {
			estimated_bitrate_bps: 318_000_000,
			bytes_per_day: 3_434_400_000_000,
			known_streams: 42,
			unknown_streams: 0,
			estimated_retention_days: 14
		}
	} satisfies SanitizedConfig;

	const names = ['Workshop', 'Till', 'Porch'];
	const cameras: CameraSettings[] = Array.from({ length: 42 }, (_, index) => ({
		id: `camera-${index + 1}`,
		ip: `192.0.2.${index + 10}`,
		display_name: names[index] ?? `Camera ${index + 1}`,
		manufacturer_override: null,
		username_configured: index < 40,
		password_configured: index < 39,
		onvif_port: null,
		http_port: null,
		main_rtsp_url: index === 1 ? `rtsp://192.0.2.${index + 10}/main` : null,
		sub_rtsp_url: null,
		uid_configured: false,
		backend: index === 1 ? 'retina' : index === 2 ? 'reo-proto' : 'auto',
		transport: index === 1 ? 'udp' : 'tcp',
		record_generic_motion_events: false,
		health: index === 1 ? 'degraded' : 'online',
		model: null
	}));
</script>

<main
	data-paper-scenario="settings.desktop.camera-defaults"
	class="h-[806px] w-[1374px] overflow-hidden bg-ground px-8 py-7 [font-synthesis:none]"
>
	<CameraDefaultsSection {cameras} {config} />
</main>
