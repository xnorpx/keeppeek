<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import NotificationsPaperFrame from '$lib/components/NotificationsPaperFrame.svelte';

	setCapabilityState();
</script>

<main
	data-paper-scenario="settings.desktop.notifications"
	class="h-[1075px] w-[1440px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<NotificationsPaperFrame />
</main>
