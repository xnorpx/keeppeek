<script lang="ts">
	import { volumeRoles, type VolumeDraft } from '$lib/storage-volumes';
	import { StorageVolumeState } from '$lib/proto/webrtc_pb';
	import { Button } from '$lib/components/ui/button/index.js';
	let {
		value = $bindable(),
		index,
		onremove
	}: { value: VolumeDraft; index: number; onremove: () => void } = $props();
</script>

<fieldset class="rounded-sm border border-hairline p-4">
	<legend class="px-2 text-sm font-medium">Volume {index + 1}</legend>
	<div class="grid gap-3 sm:grid-cols-2">
		<label
			>ID<input bind:value={value.id} required autocomplete="off" placeholder="archive" /></label
		>
		<label
			>Root or secret reference<input
				bind:value={value.root}
				required
				autocomplete="off"
				spellcheck="false"
				placeholder={'{secret:ARCHIVE_ROOT}'}
			/></label
		>
		<label
			>State<select disabled value={value.state} aria-describedby="volume-activation-note">
				<option value={StorageVolumeState.DISABLED}>Disabled draft</option>
				<option value={StorageVolumeState.ENABLED}>Enabled (existing configuration)</option>
				<option value={StorageVolumeState.READ_ONLY}>Read only (existing configuration)</option>
				<option value={StorageVolumeState.DRAINING}>Draining (existing configuration)</option>
			</select></label
		>
		<label>Priority<input bind:value={value.priority} inputmode="numeric" /></label>
		<label
			>Capacity in bytes (blank means unlimited)<input
				bind:value={value.capacityBytes}
				inputmode="numeric"
			/></label
		>
		<label
			>Minimum free bytes<input bind:value={value.minimumFreeBytes} inputmode="numeric" /></label
		>
		<label
			>Warning free bytes<input bind:value={value.warningFreeBytes} inputmode="numeric" /></label
		>
		<label
			>Critical free bytes<input bind:value={value.criticalFreeBytes} inputmode="numeric" /></label
		>
		<label
			>Allowed source IDs (one per line)<textarea bind:value={value.sources} rows="2"
			></textarea></label
		>
		<label
			>Allowed group IDs (one per line)<textarea bind:value={value.groups} rows="2"
			></textarea></label
		>
	</div>
	<fieldset class="mt-3 flex flex-wrap gap-4">
		<legend class="mb-2 text-xs text-text-muted">Roles</legend>
		{#each volumeRoles as role (role.value)}
			<label class="flex-row items-center"
				><input type="checkbox" value={role.value} bind:group={value.roles} />{role.label}</label
			>
		{/each}
	</fieldset>
	<div class="mt-3">
		<Button variant="outline" size="sm" type="button" onclick={onremove}
			>Remove volume {index + 1}</Button
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
