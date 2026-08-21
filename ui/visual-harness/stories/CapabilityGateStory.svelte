<script lang="ts">
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import { CapabilityState } from '$lib/capability-state.svelte';
	import type { ServerCapabilityId } from '$lib/capabilities';

	type Props = {
		action: string;
		capability: ServerCapabilityId;
		supported?: boolean;
	};

	let { action, capability, supported = false }: Props = $props();
</script>

<main class="grid min-h-[240px] place-items-center bg-background p-8 text-foreground">
	{#key `${capability}:${supported}`}
		{@const state = new CapabilityState(supported ? [capability] : [])}
		<CapabilityGate {action} {capability} {state}>
			{#snippet children()}
				<button
					type="button"
					class="h-8 rounded-sm bg-primary px-3 text-xs font-semibold text-primary-foreground"
				>
					{action}
				</button>
			{/snippet}
		</CapabilityGate>
	{/key}
</main>
