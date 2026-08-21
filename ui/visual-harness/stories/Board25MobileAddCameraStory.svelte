<script lang="ts">
	import type { CameraWizardDraft } from '$lib/camera-wizard';
	import MobileAddCameraWizard, {
		type MobileCameraWizardStage
	} from '$lib/components/MobileAddCameraWizard.svelte';
	import type { DiscoveredCameraSettings } from '$lib/types';
	import MobileDeviceStatusBar from './MobileDeviceStatusBar.svelte';

	type Props = { stage: MobileCameraWizardStage };
	let { stage }: Props = $props();

	const draft = {
		ip: '192.168.1.71',
		displayName: 'Side Gate',
		username: 'admin',
		password: 'write-only-password',
		onvifPort: '8000',
		httpPort: '80',
		mainRtspUrl: 'rtsp://192.168.1.71/main',
		subRtspUrl: 'rtsp://192.168.1.71/sub',
		backend: 'reo-proto',
		transport: 'tcp',
		discoveryEvidence: 'ONVIF · DS-2CD2387G2'
	} satisfies CameraWizardDraft;

	const discovered: DiscoveredCameraSettings[] = [
		{
			ip: '192.168.1.71',
			brand: 'ONVIF',
			name: null,
			model: 'DS-2CD2387G2',
			onvif_port: 8000,
			sources: [],
			configured: false,
			health: null
		},
		{
			ip: '192.168.1.41',
			brand: 'Already added',
			name: null,
			model: 'Front Door',
			onvif_port: 8000,
			sources: [],
			configured: true,
			health: 'online'
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
		subnetPrefixes="192.168.1"
		manualAddress=""
		discovering={stage === 'find-connect'}
		discoveryElapsedMs={900}
		discoveryAttempted={false}
		error={null}
		saving={false}
		actionFixed={false}
	/>
</main>
