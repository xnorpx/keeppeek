<script lang="ts">
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import BellIcon from '@lucide/svelte/icons/bell';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import SlidersIcon from '@lucide/svelte/icons/sliders-horizontal';
	import VideoIcon from '@lucide/svelte/icons/video';

	type Props = {
		active?: 'peek' | 'events' | 'cameras' | 'health' | 'settings';
		paperCompact?: boolean;
		paperFull?: boolean;
	};

	let { active = 'settings', paperCompact = false, paperFull = false }: Props = $props();
	const icons = [VideoIcon, HistoryIcon, BellIcon, CameraIcon, ActivityIcon, SlidersIcon];
	let visibleIcons = $derived(paperCompact ? icons.slice(0, 4) : icons);
	let activeIndex = $derived(
		active === 'peek'
			? 0
			: active === 'events'
				? 2
				: active === 'cameras'
					? 3
					: active === 'health'
						? 4
						: 5
	);
</script>

<aside
	data-paper-desktop-rail
	class="flex h-full w-16 shrink-0 flex-col items-center border-r border-hairline py-4 {paperFull
		? 'gap-0 bg-surface'
		: paperCompact
			? 'gap-[22px] bg-surface'
			: 'gap-2.5 bg-ground'}"
	aria-label="Desktop navigation preview"
>
	<span
		class="grid size-[30px] shrink-0 place-items-center rounded-sm bg-primary text-sm font-bold text-on-primary"
		>K</span
	>
	{#if paperFull}
		<span class="h-7 w-full shrink-0"></span>
		<div class="flex w-full shrink-0 flex-col gap-1">
			{#each icons as Icon, index (index)}
				<span
					class="grid h-11 w-16 shrink-0 place-items-center border-l-2 {index === activeIndex
						? 'border-primary text-primary-soft'
						: 'border-transparent text-text-muted'}"
				>
					<Icon class="size-5" strokeWidth={1.75} />
				</span>
			{/each}
		</div>
		<span class="w-full flex-1"></span>
		<span
			class="grid size-7 shrink-0 place-items-center rounded-full border border-hairline-strong bg-raised font-mono text-[11px] font-medium text-text-muted"
			>MA</span
		>
	{:else}
		{#if !paperCompact}<span class="h-[18px] shrink-0"></span>{/if}
		{#each visibleIcons as Icon, index (index)}
			<span
				class="grid shrink-0 place-items-center {paperCompact
					? 'size-5'
					: 'h-11 w-16 border-l-2'} {index === activeIndex
					? paperCompact
						? 'text-primary-soft'
						: 'border-primary bg-primary/10 text-primary-soft'
					: paperCompact
						? 'text-text-muted'
						: 'border-transparent text-text-faint'}"
			>
				<Icon class="size-5" strokeWidth={1.75} />
			</span>
		{/each}
	{/if}
</aside>
