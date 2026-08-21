<script lang="ts">
	import EventSourcesSection from '$lib/components/EventSourcesSection.svelte';
	import type { ServerHealthResponse } from '$lib/types';

	const health = {
		storage: {
			catalog: {
				recording_files: 1_000,
				finalized_files: 990,
				active_files: 10,
				fragments: 50_000,
				fragment_bytes: 1_800_000_000_000,
				events: 1_402,
				open_events: 2,
				event_thumbnails: 350
			}
		}
	} as ServerHealthResponse;
</script>

<main
	data-paper-scenario="settings.desktop.event-sources"
	class="h-[1048px] w-[1440px] overflow-hidden bg-ground [font-synthesis:none]"
>
	<EventSourcesSection {health} paperFrame />
</main>
