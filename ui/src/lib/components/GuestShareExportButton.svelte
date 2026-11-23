<script lang="ts">
	import { onMount } from 'svelte';
	import { capabilityActions } from '$lib/capability-actions';
	import type { ControlClient } from '$lib/control-client';
	import type { SharingControlClient } from '$lib/control-client-sharing';
	import type { RangePreparation } from '$lib/sharing-range';
	import { Button } from '$lib/components/ui/button/index.js';
	import GuestShareDialog from '$lib/components/GuestShareDialog.svelte';
	import LinkIcon from '@lucide/svelte/icons/link';

	type Props = {
		exportJobId?: string;
		range?: RangePreparation;
		controller: {
			sharing: Pick<SharingControlClient, 'getPolicy' | 'previewExport' | 'create'>;
			onAccessState: ControlClient['onAccessState'];
		};
	};
	let { exportJobId, range, controller }: Props = $props();
	let targetKey = $derived(range ? JSON.stringify(range.target) : exportJobId);
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
		if (openFor !== null && openFor !== targetKey) openFor = null;
	});
</script>

{#if identity && targetKey}
	<Button
		variant="outline"
		class="min-h-11 w-full"
		onclick={() => {
			openFor = targetKey ?? null;
		}}
		><LinkIcon class="size-4" />{range
			? capabilityActions.shareRecordingRange.action
			: capabilityActions.createGuestLink.action}</Button
	>
	{#if openFor === targetKey}
		<GuestShareDialog
			controller={controller.sharing}
			{exportJobId}
			{range}
			onclose={() => {
				openFor = null;
			}}
		/>
	{/if}
{/if}
