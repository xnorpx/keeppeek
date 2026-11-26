<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import AccessPaperFrame from '$lib/components/AccessPaperFrame.svelte';

	setCapabilityState(['keeppeek.media-export.v1']);
</script>

<main
	data-paper-scenario="settings.desktop.access"
	class="h-[1249px] w-[1440px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<AccessPaperFrame />
</main>
