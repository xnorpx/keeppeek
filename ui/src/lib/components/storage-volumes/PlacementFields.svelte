<script lang="ts">
	import { volumeRoles, type PlacementDraft } from '$lib/storage-volumes';
	import { StoragePlacementStrategy } from '$lib/proto/webrtc_pb';
	import { Button } from '$lib/components/ui/button/index.js';
	let {
		value = $bindable(),
		index,
		onremove
	}: { value: PlacementDraft; index: number; onremove: () => void } = $props();
</script>

<fieldset class="rounded-sm border border-hairline p-4">
	<legend class="px-2 text-sm font-medium">Placement rule {index + 1}</legend>
	<div class="grid gap-3 sm:grid-cols-2">
		<label
			>Role<select bind:value={value.role}
				>{#each volumeRoles as role (role.value)}<option value={role.value}>{role.label}</option
					>{/each}</select
			></label
		>
		<label
			>Strategy<select bind:value={value.strategy}
				><option value={StoragePlacementStrategy.PRIORITY}>Priority</option><option
					value={StoragePlacementStrategy.FREE_SPACE}>Most free space</option
				></select
			></label
		>
		<label>Source override (optional)<input bind:value={value.source} /></label>
		<label>Group override (optional)<input bind:value={value.group} /></label>
		<label
			>Candidate volume IDs in order (one per line)<textarea bind:value={value.candidates} rows="3"
			></textarea></label
		>
	</div>
	<label class="mt-3 flex-row items-center"
		><input type="checkbox" bind:checked={value.allowFallback} />Allow fallback within these
		candidates</label
	>
	<p class="mt-2 text-xs text-text-muted">
		Leave both selectors empty for the role default. Source overrides take precedence over group
		overrides.
	</p>
	<div class="mt-3">
		<Button variant="outline" size="sm" type="button" onclick={onremove}
			>Remove rule {index + 1}</Button
		>
	</div>
</fieldset>

<style>
	label {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
		font-size: 0.75rem;
	}
	input:not([type='checkbox']),
	select,
	textarea {
		width: 100%;
		min-width: 0;
		border: 1px solid var(--color-hairline);
		background: var(--color-surface);
		color: inherit;
		border-radius: 0.125rem;
		padding: 0.5rem;
		font-size: 0.875rem;
	}
	input:focus-visible,
	select:focus-visible,
	textarea:focus-visible {
		outline: 2px solid var(--color-activity);
		outline-offset: 2px;
	}
	label.flex-row {
		flex-direction: row;
	}
</style>
