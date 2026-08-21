<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import AccessSection from '$lib/components/AccessSection.svelte';

	setCapabilityState(['keeppeek.media-export.v1']);
</script>

<main
	data-paper-scenario="settings.desktop.access"
	class="h-[1249px] w-[1440px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<AccessSection paperFrame />
</main>
