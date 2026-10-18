<script lang="ts">
	import { onMount } from 'svelte';
	import { capabilityActions } from '$lib/capability-actions';
	import type { ControlClient } from '$lib/control-client';
	import type { SharingControlClient } from '$lib/control-client-sharing';
	import { Button } from '$lib/components/ui/button/index.js';
	import GuestShareDialog from '$lib/components/GuestShareDialog.svelte';
	import LinkIcon from '@lucide/svelte/icons/link';

	type Props = {
		exportJobId: string;
		controller: {
			sharing: Pick<SharingControlClient, 'getPolicy' | 'previewExport' | 'create'>;
			onAccessState: ControlClient['onAccessState'];
		};
	};
	let { exportJobId, controller }: Props = $props();
	let identity = $state<string | null>(null);
	let openFor = $state<string | null>(null);

	onMount(() =>
		controller.onAccessState((state) => {
			const next =
				state.status === 'authenticated' && state.session?.role === 'administrator'
					? `${state.session.principalId}:${state.generation}`
					: null;
			if (next !== identity) openFor = null;
			identity = next;
		})
	);
	$effect.pre(() => {
		if (openFor !== null && openFor !== exportJobId) openFor = null;
	});
</script>

{#if identity}
	<Button
		variant="outline"
		class="min-h-11 w-full"
		onclick={() => {
			openFor = exportJobId;
		}}><LinkIcon class="size-4" />{capabilityActions.createGuestLink.action}</Button
	>
	{#if openFor === exportJobId}
		<GuestShareDialog
			controller={controller.sharing}
			{exportJobId}
			onclose={() => {
				openFor = null;
			}}
		/>
	{/if}
{/if}
