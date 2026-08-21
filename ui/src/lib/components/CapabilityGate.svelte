<script lang="ts">
	import type { Snippet } from 'svelte';
	import { useCapabilityState } from '$lib/capability-context';
	import type { CapabilityState } from '$lib/capability-state.svelte';
	import type { ServerCapabilityId } from '$lib/capabilities';
	import { cn } from '$lib/utils';

	type Props = {
		action: string;
		capability: ServerCapabilityId;
		children?: Snippet;
		class?: string;
		state?: CapabilityState;
	};

	let {
		action,
		capability,
		children,
		class: className,
		state = useCapabilityState()
	}: Props = $props();
</script>

{#if state.supports(capability)}
	{@render children?.()}
{:else}
	<div
		data-capability-gate
		data-capability={capability}
		class={cn(
			'inline-flex min-h-8 max-w-full items-center gap-2 rounded-sm border border-border bg-muted px-2.5 py-1 font-mono text-xs text-muted-foreground',
			className
		)}
		role="status"
	>
		<span class="font-semibold text-foreground">{action}</span>
		<span class="min-w-0 truncate">{state.label(capability)}</span>
	</div>
{/if}
