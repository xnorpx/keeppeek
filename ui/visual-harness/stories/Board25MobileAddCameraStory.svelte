<script lang="ts">
	import type { CameraWizardDraft } from '$lib/camera-wizard';
	import MobileAddCameraWizard, {
		type MobileCameraWizardStage
	} from '$lib/components/MobileAddCameraWizard.svelte';
	import type {
		CameraCatalogCamera,
		CameraCatalogInfo,
		DiscoveredCameraSettings
	} from '$lib/types';
	import MobileDeviceStatusBar from './MobileDeviceStatusBar.svelte';

	type Props = { stage: MobileCameraWizardStage };
	let { stage }: Props = $props();

	const draft = {
		ip: '192.168.1.71',
		displayName: 'Side Gate',
		username: 'admin',
		password: 'write-only-password',
		defaultUsernameConfigured: false,
		defaultPasswordConfigured: false,
		onvifPort: '8000',
		httpPort: '80',
		mainRtspUrl: 'rtsp://192.168.1.71/main',
		subRtspUrl: 'rtsp://192.168.1.71/sub',
		backend: 'reo-proto',
		transport: 'tcp',
		recordGenericMotionEvents: false,
		recordingMode: 'event-boost',
		eventRecordingDurationSeconds: '60',
		discoveryEvidence: 'ONVIF · DS-2CD2387G2'
	} satisfies CameraWizardDraft;

	const catalogInfo = {
		version: '2.1.0',
		tag: 'v2.1.0',
		generated_at: '2026-08-22T06:13:00Z',
		camera_count: 3433,
		website_url: 'https://www.cctv-database.com/'
	} satisfies CameraCatalogInfo;

	const catalogCamera = {
		id: 'hikvision-ds-2cd2387g2',
		brand: 'Hikvision',
		model: 'DS-2CD2387G2',
		aliases: [],
		camera_type: 'dome',
		resolution_label: '4K',
		megapixels: 8,
		sensor: null,
		field_of_view: null,
		night_vision: 'IR',
		ip_rating: 'IP67',
		ik_rating: null,
		two_way_audio: false,
		release_year: null,
		community_notes_count: 0,
		protocols: ['onvif', 'rtsp'],
		codecs: ['H.265', 'H.264'],
		streams: [
			{ name: 'main', resolution: '3840x2160', fps: 25, codec: 'H.265' },
			{ name: 'sub', resolution: '640x360', fps: 10, codec: 'H.264' }
		],
		sources: ['https://www.cctv-database.com/'],
		stream_hints: {
			main_rtsp_url: 'rtsp://192.168.1.71/main',
			sub_rtsp_url: 'rtsp://192.168.1.71/sub'
		}
	} satisfies CameraCatalogCamera;

	const discovered: DiscoveredCameraSettings[] = [
		{
			ip: '192.168.1.71',
			brand: 'ONVIF',
			name: null,
			model: 'DS-2CD2387G2',
			onvif_port: 8000,
			sources: [],
			configured: false,
			health: null,
			catalog: catalogCamera
		},
		{
			ip: '192.168.1.41',
			brand: 'Already added',
			name: null,
			model: 'Front Door',
			onvif_port: 8000,
			sources: [],
			configured: true,
			health: 'healthy'
		}
	];

	const scenarioIds: Record<MobileCameraWizardStage, string> = {
		'find-connect': 'cameras.mobile.add-wizard',
		streams: 'cameras.mobile.add-streams',
		review: 'cameras.mobile.add-review'
	};
</script>

<main
	data-paper-scenario={scenarioIds[stage]}
	class="flex h-[844px] w-[390px] flex-col overflow-hidden rounded-lg border border-hairline-strong bg-ground [font-synthesis:none]"
>
	<MobileDeviceStatusBar />
	<MobileAddCameraWizard
		{stage}
		{draft}
		{discovered}
		selectedCatalogCamera={catalogCamera}
		{catalogInfo}
		subnetPrefixes="192.168.1"
		manualAddress=""
		discovering={stage === 'find-connect'}
		discoveryElapsedMs={900}
		discoveryAttempted={false}
		error={null}
		saving={false}
		streamResolution="onvif"
		catalogStreamsApplied={stage === 'streams'}
		actionFixed={false}
	/>
</main>
