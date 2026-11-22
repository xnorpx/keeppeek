<script lang="ts">
	import { onMount } from 'svelte';
	import type { ControlClient } from '$lib/control-client';
	import GuestSharingPolicy from '$lib/components/GuestSharingPolicy.svelte';
	import GuestShareGrants from '$lib/components/GuestShareGrants.svelte';

	let { controller }: { controller: Pick<ControlClient, 'sharing' | 'onAccessState'> } = $props();
	let identity = $state<string | null>(null);
	onMount(() =>
		controller.onAccessState((state) => {
			identity =
				state.status === 'authenticated' && state.session?.role === 'administrator'
					? `${state.session.principalId}:${state.generation}`
					: null;
		})
	);
</script>

{#if identity}
	{#key identity}
		<section
			class="mt-6 space-y-6 rounded-md border border-border bg-background p-4 sm:p-6"
			aria-label="Guest sharing"
		>
			<GuestSharingPolicy controller={controller.sharing} />
			<GuestShareGrants controller={controller.sharing} />
		</section>
	{/key}
{/if}
