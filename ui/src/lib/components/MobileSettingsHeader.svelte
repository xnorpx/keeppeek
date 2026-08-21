<script lang="ts">
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';

	type Props = {
		title: string;
		backHref?: string;
		trailing?: string;
	};

	let { title, backHref, trailing }: Props = $props();
</script>

<header
	data-mobile-settings-header
	class="flex h-[52px] shrink-0 items-center justify-between gap-3 border-b border-hairline bg-background px-4 md:hidden"
>
	<div class="flex min-w-0 items-center gap-3">
		{#if backHref}
			<a
				href={backHref}
				class="grid size-[18px] shrink-0 place-items-center text-text focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				aria-label="Back to settings sections"
			>
				<ChevronLeftIcon class="size-[18px]" strokeWidth={2} />
			</a>
		{/if}
		<h1
			class="truncate {backHref
				? 'text-lg leading-5 font-semibold'
				: 'text-xl leading-6 font-bold'}"
		>
			{title}
		</h1>
	</div>
	{#if trailing}
		<span class="shrink-0 font-mono text-2xs leading-3 text-text-faint uppercase">{trailing}</span>
	{:else}
		<span
			class="grid size-7 shrink-0 place-items-center rounded-full bg-primary text-xs leading-[14px] font-semibold text-on-primary"
			aria-label="Local connection"
			title="Local connection"
		>
			L
		</span>
	{/if}
</header>
