<script lang="ts">
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import type { ServerCapabilityId } from '$lib/capabilities';

	type Props = {
		action: string;
		capability: ServerCapabilityId;
		onaction?: () => void;
		fixed?: boolean;
	};

	let { action, capability, onaction, fixed = true }: Props = $props();
</script>

<footer
	data-mobile-settings-action-bar
	class="{fixed
		? 'fixed inset-x-0 bottom-0 z-50'
		: 'relative'} flex h-[68px] shrink-0 items-center justify-center border-t border-hairline bg-surface px-4 md:hidden"
>
	<CapabilityGate
		{action}
		{capability}
		class="h-[38px] min-h-0 max-w-full justify-center px-3 text-sm"
	>
		{#snippet children()}
			<Button class="h-[38px] w-full max-w-sm" onclick={() => onaction?.()}>{action}</Button>
		{/snippet}
	</CapabilityGate>
</footer>
