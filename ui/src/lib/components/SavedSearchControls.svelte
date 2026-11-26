<script lang="ts">
	import type { SavedEventSearch } from '$lib/control-client';
	import BookmarkIcon from '@lucide/svelte/icons/bookmark';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';

	type Props = {
		searches: readonly SavedEventSearch[];
		activeSearchId: string | null;
		draftValid: boolean;
		busy?: boolean;
		error?: string | null;
		onload: (search: SavedEventSearch) => void;
		onsave: (name: string, existing: SavedEventSearch | null) => void;
		ondelete: (search: SavedEventSearch) => void;
	};

	let {
		searches,
		activeSearchId,
		draftValid,
		busy = false,
		error = null,
		onload,
		onsave,
		ondelete
	}: Props = $props();

	let activeSearch = $derived(searches.find((item) => item.searchId === activeSearchId) ?? null);
	let searchName = $state('');

	$effect(() => {
		if (activeSearch) searchName = activeSearch.name;
	});

	function submitSave(event: SubmitEvent): void {
		event.preventDefault();
		const trimmed = searchName.trim();
		if (!trimmed || !draftValid || busy) return;
		onsave(trimmed, activeSearch?.editable ? activeSearch : null);
	}
</script>

<section
	class="space-y-2 rounded-md border border-hairline bg-surface p-3"
	aria-label="Saved event searches"
	data-testid="saved-search-controls"
>
	<div class="flex flex-wrap items-center justify-between gap-2">
		<h2 class="flex items-center gap-1.5 text-xs font-semibold">
			<BookmarkIcon class="size-3.5" /> Saved searches
		</h2>
		<form class="flex flex-wrap items-center gap-2" onsubmit={submitSave}>
			<label class="sr-only" for="saved-search-name">Saved search name</label>
			<input
				id="saved-search-name"
				type="text"
				bind:value={searchName}
				maxlength="128"
				placeholder="Name this search…"
				class="h-8 w-44 rounded-sm border border-hairline bg-raised px-2 text-xs"
			/>
			<button
				type="submit"
				class="h-8 rounded-sm border border-hairline bg-raised px-2.5 text-xs font-medium hover:bg-surface disabled:opacity-50"
				disabled={!draftValid || !searchName.trim() || busy}
			>
				{activeSearch?.editable ? `Update rev ${activeSearch.revision}` : 'Save search'}
			</button>
		</form>
	</div>

	{#if error}
		<p
			role="alert"
			class="rounded-sm border border-destructive/40 bg-destructive/10 px-2.5 py-1.5 text-xs text-destructive"
			data-testid="saved-search-error"
		>
			{error}
		</p>
	{/if}

	{#if searches.length > 0}
		<ul class="flex flex-wrap gap-2">
			{#each searches as item (item.searchId)}
				<li
					class="flex items-center gap-1 rounded-sm border border-hairline bg-ground px-2 py-1 text-xs"
				>
					<button type="button" class="font-medium hover:underline" onclick={() => onload(item)}>
						{item.name}
					</button>
					<span class="font-mono text-2xs text-text-faint">r{item.revision}</span>
					{#if !item.editable}
						<span class="rounded-sm bg-raised px-1 font-mono text-2xs text-text-muted">
							v{item.expressionVersion} read-only
						</span>
					{/if}
					{#if item.tombstonedReferences.length > 0}
						<span
							class="rounded-sm bg-destructive/15 px-1 font-mono text-2xs text-destructive"
							title={`Missing references: ${item.tombstonedReferences.join(', ')}`}
						>
							{item.tombstonedReferences.length} missing ref
						</span>
					{/if}
					<button
						type="button"
						class="grid size-6 place-items-center rounded-sm text-text-muted hover:bg-raised hover:text-destructive"
						aria-label={`Delete saved search ${item.name}`}
						disabled={busy}
						onclick={() => ondelete(item)}
					>
						<Trash2Icon class="size-3" />
					</button>
				</li>
			{/each}
		</ul>
	{/if}
</section>
