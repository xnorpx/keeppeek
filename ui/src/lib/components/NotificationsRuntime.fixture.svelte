<script lang="ts">
	import { untrack } from 'svelte';
	import { setCapabilityState } from '$lib/capability-context';
	import { setControlClient } from '$lib/control-context';
	import type { ControlClient } from '$lib/control-client';
	import NotificationsSection from './NotificationsSection.svelte';

	let { client }: { client: ControlClient } = $props();

	setCapabilityState(['keeppeek.rules.v1']);
	setControlClient(untrack(() => client));
</script>

<NotificationsSection />
