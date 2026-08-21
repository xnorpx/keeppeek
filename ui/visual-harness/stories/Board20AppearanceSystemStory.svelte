<script lang="ts">
	import { setAppearanceState } from '$lib/appearance-context';
	import AppearanceSystemSection from '$lib/components/AppearanceSystemSection.svelte';
	import type { ServerHealthResponse } from '$lib/types';

	setAppearanceState();

	const health = {
		version: '0.4.1-pre · 9f2ac1d',
		uptime_seconds: 1_836_840,
		system: {
			os_name: 'macOS',
			os_version: 'macOS 15.4',
			process: {
				name: 'Native service',
				executable: '/opt/keeppeek/bin/keeppeek',
				working_directory: '/opt/keeppeek'
			}
		}
	} as ServerHealthResponse;

	const logLines = [
		'06:37:23.114  INFO   rtsp     front-door keyframe 1.1s',
		'06:37:22.902  INFO   storage  fragment flushed 4.2 MB',
		'06:37:20.441  WARN   rtsp     porch rtp gap 41022..41029',
		'06:37:19.118  ERROR  rtsp     back-yard connect refused',
		'06:37:19.118  INFO   rtsp     back-yard backoff 30s try 28',
		'06:37:11.004  INFO   webrtc   session 7c1a  8 tracks',
		'06:37:04.870  INFO   events   person 0.91 front-door'
	];
</script>

<main
	data-paper-scenario="settings.desktop.appearance-system-logs"
	class="h-[581px] w-[1440px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<AppearanceSystemSection
		{health}
		onrestart={() => {}}
		paperFrame
		timeZoneOverride="Europe/Stockholm"
		reducedMotionOverride
		{logLines}
		logsLive
		logTarget="rtsp"
	/>
</main>
