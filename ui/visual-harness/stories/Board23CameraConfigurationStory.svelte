<script lang="ts">
	import CameraConfigurationEditor from '$lib/components/CameraConfigurationEditor.svelte';
	import type { CameraSettings } from '$lib/types';

	const camera = {
		id: 'front-door',
		ip: '192.168.1.42',
		display_name: 'Front Door',
		manufacturer_override: null,
		username_configured: true,
		password_configured: true,
		onvif_port: 8000,
		http_port: 80,
		main_rtsp_url: 'rtsp://192.168.1.42:554/h265Preview_01_main',
		sub_rtsp_url: 'rtsp://192.168.1.42:554/h264Preview_01_sub',
		uid_configured: true,
		backend: 'reo-proto',
		transport: 'tcp',
		record_generic_motion_events: false,
		recording_mode: 'event-boost',
		event_recording_duration_secs: 60,
		health: 'healthy',
		model: 'RLC-811A'
	} satisfies CameraSettings;

	const sections = [
		'Overview',
		'Configuration',
		'Connection',
		'Events',
		'Streams',
		'Audio',
		'Advanced'
	];
</script>

<main
	data-paper-scenario="camera.desktop.configuration"
	class="flex h-[806px] w-[1374px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<aside class="h-[806px] w-[168px] shrink-0 border-r border-hairline px-4 py-7">
		<p class="px-2 font-mono text-2xs tracking-caps text-text-faint">THIS CAMERA</p>
		<nav class="mt-2 flex flex-col" aria-label="Camera sections preview">
			{#each sections as section (section)}
				<span
					class="flex h-[34px] items-center border-l-2 px-2 text-xs {section === 'Configuration'
						? 'border-primary bg-primary/10 font-semibold text-foreground'
						: 'border-transparent text-text-muted'}"
				>
					{section}
				</span>
			{/each}
		</nav>
	</aside>
	<div class="h-[806px] w-[1206px] overflow-hidden px-7 py-6">
		<header class="mb-[14px] flex h-[66px] items-start justify-between gap-4">
			<div>
				<p class="font-mono text-2xs tracking-caps text-primary-soft">EDIT THIS CAMERA</p>
				<h1 class="mt-1 text-2xl font-semibold">Front Door configuration</h1>
				<p class="mt-1 text-xs text-text-muted">
					Connection, credentials, streams, and recording policy.
				</p>
			</div>
			<span
				class="rounded-sm border border-activity/40 bg-activity/10 px-2.5 py-1.5 font-mono text-2xs text-activity"
			>
				SAVES TO CONFIG · RESTART REQUIRED
			</span>
		</header>
		<CameraConfigurationEditor {camera} oncancel={() => {}} onsave={() => {}} />
	</div>
</main>
