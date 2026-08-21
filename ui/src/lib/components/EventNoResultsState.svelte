<script lang="ts">
	type Clause = {
		label: string;
		constraining?: boolean;
	};

	type Props = {
		clauses: readonly Clause[];
		title: string;
		description: string;
		suggestionLabel?: string | null;
		onloosen?: () => void;
		onclear: () => void;
		class?: string;
	};

	let {
		clauses,
		title,
		description,
		suggestionLabel = null,
		onloosen,
		onclear,
		class: className = ''
	}: Props = $props();
</script>

<div data-event-no-results class="flex flex-col gap-3.5 bg-raised p-[18px] {className}">
	<div class="flex flex-wrap items-center gap-1.5">
		{#each clauses as clause (clause.label)}
			<span
				class="inline-flex h-6 items-center rounded-sm border px-[9px] font-mono text-2xs leading-3 {clause.constraining
					? 'border-live/40 bg-live/10 text-live-text'
					: 'border-hairline-strong bg-surface'}"
			>
				{clause.label}
			</span>
		{/each}
	</div>
	<h2 class="text-xl leading-6 font-semibold">{title}</h2>
	<p class="text-sm leading-[19px] text-text-muted">{description}</p>
	<div class="flex items-center gap-2.5">
		{#if suggestionLabel && onloosen}
			<button
				type="button"
				class="h-8 rounded-sm bg-primary px-3.5 text-sm leading-4 font-semibold text-on-primary"
				onclick={onloosen}
			>
				{suggestionLabel}
			</button>
		{/if}
		<button
			type="button"
			class="h-8 rounded-sm border border-hairline-strong bg-surface px-3.5 text-sm leading-4"
			onclick={onclear}
		>
			Clear filters
		</button>
	</div>
</div>
