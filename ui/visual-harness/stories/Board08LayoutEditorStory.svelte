<script lang="ts">
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import PeekLayoutEditor from '$lib/components/PeekLayoutEditor.svelte';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type { CameraHealth, CameraListItem } from '$lib/types';

	setLivePeer();

	const cameraNames = [
		['front-door', 'Front Door'],
		['driveway', 'Driveway'],
		['back-yard', 'Back Yard'],
		['side-gate', 'Side Gate'],
		['workshop', 'Workshop'],
		['yard-ptz', 'Yard PTZ']
	] as const;
	const cameras: CameraListItem[] = cameraNames.map(([id, name], index) => ({
		id,
		ip: `192.0.2.${index + 61}`,
		name,
		manufacturer: 'ONVIF',
		model: `Camera ${index + 1}`,
		firmware_version: null,
		is_reolink: false,
		profiles: []
	}));
	const healthById = new Map<string, CameraHealth>(
		cameras.map(
			(camera) =>
				[
					camera.id,
					{
						id: camera.id,
						ip: camera.ip,
						name: camera.name ?? camera.id,
						manufacturer: camera.manufacturer,
						model: camera.model,
						firmware_version: null,
						state: camera.id === 'workshop' ? 'offline' : 'healthy',
						lifecycle: camera.id === 'workshop' ? 'Stopped' : 'Connected',
						last_error: camera.id === 'workshop' ? 'Stream unavailable' : null,
						configured_profiles: [],
						streams: []
					}
				] as const
		)
	);
</script>

<main
	data-paper-scenario="peek.desktop.layout-editor"
	class="flex h-[840px] w-[1440px] shrink-0 overflow-hidden rounded-lg border border-hairline bg-ground [font-synthesis:none]"
>
	<DesktopPaperRail active="peek" paperCompact />
	<PeekLayoutEditor
		{cameras}
		{healthById}
		streamFor={(camera) => (camera.id === 'front-door' ? 'main' : 'sub')}
		ondiscard={() => {}}
		paperFrame
	/>
</main>
