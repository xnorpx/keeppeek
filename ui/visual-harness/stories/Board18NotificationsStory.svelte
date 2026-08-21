<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import NotificationsSection from '$lib/components/NotificationsSection.svelte';

	setCapabilityState();
</script>

<main
	data-paper-scenario="settings.desktop.notifications"
	class="h-[1075px] w-[1440px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<NotificationsSection paperFrame />
</main>
