<script lang="ts">
	import DesktopCameraPaperFrame from '$lib/components/DesktopCameraPaperFrame.svelte';
	import { setControlClient } from '$lib/control-context';
	import { ControlClient, type PtzPreset } from '$lib/control-client';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type { CameraHealth, CameraListItem } from '$lib/types';

	const presets: PtzPreset[] = [
		{ id: 1, name: 'Front step' },
		{ id: 2, name: 'Driveway gate' },
		{ id: 3, name: 'Side path' }
	];

	class CameraStoryClient extends ControlClient {
		override async getPtzPresets(): Promise<PtzPreset[]> {
			return presets;
		}
	}

	setControlClient(new CameraStoryClient());
	setLivePeer();

	const camera: CameraListItem = {
		id: 'front-door',
		ip: '192.168.1.42',
		name: 'Front Door',
		manufacturer: 'Reolink',
		model: 'RLC-811A',
		firmware_version: 'v3.1.0',
		serial_number: 'REO-0001',
		hardware_id: 'RLC-811A',
		hostname: 'front-door',
		mac_address: '02:00:00:00:00:42',
		is_reolink: true,
		backend: 'reo-proto',
		transport: 'tcp',
		ports: { http: 80, https: null, rtsp: 554, onvif: 8000 },
		capabilities: {
			ptz: true,
			audio: true,
			events: true,
			recording: true,
			analytics: false,
			imaging: true,
			two_way_audio: true
		},
		profiles: [
			{
				name: 'mainStream',
				stream: 'main',
				encoding: 'h265',
				resolution: '3840x2160',
				framerate: 25,
				bitrate_kbps: 8_192,
				gop: 50,
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
	};
	const health: CameraHealth = {
		id: camera.id,
		ip: camera.ip,
		name: camera.name ?? camera.id,
		manufacturer: camera.manufacturer,
		model: camera.model,
		firmware_version: camera.firmware_version,
		backend: camera.backend,
		transport: camera.transport,
		state: 'online',
		lifecycle: 'Connected',
		last_error: null,
		configured_profiles: camera.profiles,
		streams: [
			{
				type: 'video_main',
				codec: 'h265',
				resolution: '3840x2160',
				fps: 25,
				expected_fps: 25,
				kbps: 8_192,
				frames: 12_500,
				drops: 0,
				reconnects: 2,
				updated_at_ms: Date.parse('2026-08-18T06:37:23Z'),
				report_age_ms: 12
			}
		]
	};
</script>

<div
	data-paper-scenario="camera.desktop.details-ptz"
	class="h-[2059px] w-[1440px] shrink-0 [font-synthesis:none]"
>
	<DesktopCameraPaperFrame {camera} {health} {presets} />
</div>
