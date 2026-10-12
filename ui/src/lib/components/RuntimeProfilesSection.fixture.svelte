<script lang="ts">
	import { untrack } from 'svelte';
	import { setControlClient } from '$lib/control-context';
	import type { ControlClient } from '$lib/control-client';
	import type { CameraSettings } from '$lib/types';
	import RuntimeProfilesSection from './RuntimeProfilesSection.svelte';

	let { client, cameras }: { client: ControlClient; cameras: CameraSettings[] } = $props();

	setControlClient(untrack(() => client));
</script>

<RuntimeProfilesSection {cameras} administrator={true} />
