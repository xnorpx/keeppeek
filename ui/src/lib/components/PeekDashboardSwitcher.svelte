<script lang="ts">
	import { Popover } from 'bits-ui';
	import type { PeekLayout } from '$lib/peek-layout';
	import CheckIcon from '@lucide/svelte/icons/check';
	import ChevronDownIcon from '@lucide/svelte/icons/chevron-down';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';

	type Props = {
		layouts: readonly PeekLayout[];
		activeLayout: PeekLayout | null;
		busy: boolean;
		onselect: (dashboardId: string) => Promise<void>;
	};

	let { layouts, activeLayout, busy, onselect }: Props = $props();
	let open = $state(false);
	let activeName = $derived(activeLayout?.name ?? 'All cameras');

	function choose(dashboardId: string): void {
		open = false;
		if (dashboardId === activeLayout?.id) return;
		void onselect(dashboardId);
	}
</script>

<div
	data-peek-dashboard-switcher
	class="absolute top-3 left-1/2 z-30 flex h-8 max-w-[calc(100%-1.5rem)] -translate-x-1/2 items-center overflow-hidden rounded-sm bg-video/70 text-white shadow-md ring-1 ring-white/10 backdrop-blur-md"
>
	<Popover.Root bind:open>
		<Popover.Trigger
			type="button"
			class="flex h-full min-w-0 items-center gap-2 px-2.5 text-left text-xs font-medium text-white/90 transition-colors hover:bg-white/10 focus-visible:bg-white/10 focus-visible:outline-none disabled:opacity-60"
			disabled={busy || activeLayout === null}
			aria-label={`Choose dashboard, ${activeName}`}
		>
			<span class="max-w-48 truncate">{activeName}</span>
			<ChevronDownIcon class="size-3.5 shrink-0 text-white/55" />
		</Popover.Trigger>
		<Popover.Portal>
			<Popover.Content
				role="menu"
				aria-label="Choose dashboard"
				side="bottom"
				align="start"
				sideOffset={5}
				collisionPadding={8}
				trapFocus={false}
				class="z-50 max-w-[calc(100vw-1rem)] min-w-52 overflow-hidden rounded-md border border-hairline-strong bg-popover p-1 text-popover-foreground shadow-xl"
			>
				{#each layouts as layout (layout.id)}
					<button
						type="button"
						role="menuitemradio"
						aria-checked={layout.id === activeLayout?.id}
						class="flex h-9 w-full items-center gap-2 rounded-sm px-2.5 text-left text-xs hover:bg-accent focus-visible:bg-accent focus-visible:outline-none {layout.id ===
						activeLayout?.id
							? 'bg-accent/70'
							: ''}"
						onclick={() => choose(layout.id)}
					>
						<Grid2X2Icon class="size-3.5 shrink-0 text-muted-foreground" />
						<span class="min-w-0 flex-1 truncate font-medium">{layout.name}</span>
						{#if layout.id === activeLayout?.id}
							<CheckIcon class="size-3.5 shrink-0 text-primary" />
						{/if}
					</button>
				{/each}
			</Popover.Content>
		</Popover.Portal>
	</Popover.Root>
</div>
