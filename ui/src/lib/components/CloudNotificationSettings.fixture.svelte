<script lang="ts">
	import { untrack } from 'svelte';
	import { setCapabilityState } from '$lib/capability-context';
	import { setControlClient } from '$lib/control-context';
	import type { ControlClient } from '$lib/control-client';
	import CloudNotificationSettings from './CloudNotificationSettings.svelte';

	let {
		client,
		capabilities = ['keeppeek.cloud-notifications.v1']
	}: { client: ControlClient; capabilities?: string[] } = $props();

	setCapabilityState(untrack(() => capabilities));
	setControlClient(untrack(() => client));
</script>

<CloudNotificationSettings />
