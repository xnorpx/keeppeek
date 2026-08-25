<script lang="ts">
	import MobileCameraPage, { type MobileCameraMode } from '$lib/components/MobileCameraPage.svelte';
	import CameraConfigurationEditor from '$lib/components/CameraConfigurationEditor.svelte';
	import { setControlClient } from '$lib/control-context';
	import { ControlClient } from '$lib/control-client';
	import type { CameraHealth, CameraListItem, CameraSettings } from '$lib/types';
	import MobileDeviceStatusBar from './MobileDeviceStatusBar.svelte';

	type Props = { mode: MobileCameraMode };
	let { mode }: Props = $props();

	class CameraStoryClient extends ControlClient {
		override async getPtzPresets() {
			return [
				{ id: 1, name: 'Front step' },
				{ id: 2, name: 'Driveway gate' },
				{ id: 3, name: 'Side path' }
			];
		}

		override async movePtz(): Promise<void> {}
		override async stopPtz(): Promise<void> {}
		override async gotoPtzPreset(): Promise<void> {}
	}

	setControlClient(new CameraStoryClient());

	const camera = {
		id: 'front-door',
		ip: '192.168.1.42',
		name: 'Front Door',
		manufacturer: 'Reolink',
		model: 'RLC-823A',
		firmware_version: 'v1',
		serial_number: null,
		hardware_id: null,
		hostname: 'front-door',
		mac_address: null,
		is_reolink: true,
		backend: 'reo-proto',
		transport: 'tcp',
		web_url: 'http://192.168.1.42',
		ports: { http: 80, https: null, rtsp: 554, onvif: 8000 },
		capabilities: {
			ptz: true,
			audio: true,
			events: true,
			recording: true,
			analytics: true,
			imaging: true,
			two_way_audio: true
		},
		profiles: [
			{
				name: 'mainStream',
				stream: 'main',
				encoding: 'h265',
				resolution: '3840x2160',
				framerate: 15,
				bitrate_kbps: 18_400,
				gop: 30,
				h264_profile: null,
				audio: { encoding: 'aac', sample_rate: 16_000, bitrate_kbps: 64 }
			},
			{
				name: 'subStream',
				stream: 'sub',
				encoding: 'h264',
				resolution: '640x360',
				framerate: 15,
				bitrate_kbps: 600,
				gop: 30,
				h264_profile: 'baseline',
				audio: null
			}
		]
	} satisfies CameraListItem;

	const health = {
		id: camera.id,
		ip: camera.ip,
		name: camera.name,
		manufacturer: camera.manufacturer,
		model: camera.model,
		firmware_version: camera.firmware_version,
		backend: camera.backend,
		transport: camera.transport,
		state: 'healthy',
		lifecycle: 'connected',
		last_error: null,
		configured_profiles: camera.profiles,
		streams: [
			{
				type: 'video_main',
				codec: 'h265',
				resolution: '3840x2160',
				fps: 15,
				expected_fps: 15,
				kbps: 18_400,
				frames: 54_000,
				drops: 0,
				errors: 0,
				reconnects: 0,
				updated_at_ms: Date.parse('2026-08-18T06:51:52Z'),
				report_age_ms: 120
			}
		]
	} satisfies CameraHealth;

	const settings = {
		id: camera.id,
		ip: camera.ip,
		display_name: camera.name,
		manufacturer_override: null,
		username_configured: true,
		password_configured: true,
		onvif_port: camera.ports.onvif,
		http_port: camera.ports.http,
		main_rtsp_url: 'rtsp://192.168.1.42:554/h265Preview_01_main',
		sub_rtsp_url: 'rtsp://192.168.1.42:554/h264Preview_01_sub',
		uid_configured: true,
		backend: 'reo-proto',
		transport: 'tcp',
		record_generic_motion_events: false,
		recording_mode: 'event-boost',
		event_recording_duration_secs: 60,
		health: 'healthy',
		model: camera.model
	} satisfies CameraSettings;

	const scenarioIds: Record<MobileCameraMode, string> = {
		live: 'camera.mobile.details-ptz',
		ptz: 'camera.mobile.ptz',
		settings: 'camera.mobile.settings'
	};
</script>

<main
	data-paper-scenario={scenarioIds[mode]}
	class="flex h-[844px] w-[390px] flex-col overflow-hidden rounded-lg border border-hairline-strong bg-ground [font-synthesis:none]"
>
	<MobileDeviceStatusBar />
	{#if mode === 'settings'}
		<div data-mobile-camera-configuration class="h-[780px] overflow-y-auto p-3">
			<CameraConfigurationEditor camera={settings} oncancel={() => {}} onsave={() => {}} />
		</div>
	{:else}
		<MobileCameraPage
			{camera}
			{health}
			stream="main"
			previewAvailable={false}
			commandTransportAvailable
			{mode}
			paperFrame
		/>
	{/if}
</main>
