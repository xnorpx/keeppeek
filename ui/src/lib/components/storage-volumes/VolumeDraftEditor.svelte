<script lang="ts">
	import { untrack } from 'svelte';
	import type { SanitizedConfig, SettingsConfigUpdateResponse } from '$lib/types';
	import {
		draftError,
		encodeDraft,
		newPlacement,
		newVolume,
		volumeDraft,
		type VolumeController
	} from '$lib/storage-volumes';
	import { Button } from '$lib/components/ui/button/index.js';
	import VolumeFields from './VolumeFields.svelte';
	import PlacementFields from './PlacementFields.svelte';
	let {
		config,
		controller,
		onsaved,
		oncancel
	}: {
		config: SanitizedConfig;
		controller: VolumeController;
		onsaved: (result: SettingsConfigUpdateResponse) => void;
		oncancel: () => void;
	} = $props();
	const snapshot = untrack(() => config);
	const initial = volumeDraft(snapshot.storage.named_volumes);
	let draft = $state(volumeDraft(snapshot.storage.named_volumes));
	const persistedVolumes = untrack(
		() => new Map(draft.volumes.map((volume) => [volume, volume.id]))
	);
	let saving = $state(false);
	let error = $state<string | null>(null);
	let dirty = $derived(JSON.stringify(draft) !== JSON.stringify(initial));
	let validation = $derived(draftError(draft));
	async function save(event: SubmitEvent) {
		event.preventDefault();
		if (saving || !dirty || validation) return;
		saving = true;
		error = null;
		try {
			const result = await controller.updateRuntimeConfiguration({
				host: snapshot.host,
				port: snapshot.port,
				expected_configuration_revision: snapshot.configuration_revision,
				move_existing_recordings: false,
				storage: { ...snapshot.storage, named_volumes: encodeDraft(draft) }
			});
			onsaved(result);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Volume draft was not saved.';
		} finally {
			saving = false;
		}
	}
	function cancel() {
		if (!dirty || window.confirm('Discard your unsaved volume draft?')) oncancel();
	}
	function removeVolume(index: number) {
		const originalId = persistedVolumes.get(draft.volumes[index]);
		if (
			originalId !== undefined &&
			!window.confirm(
				`Remove the saved volume definition "${originalId}" from this draft? Save the draft to apply this change. This does not delete its files.`
			)
		)
			return;
		draft.volumes = draft.volumes.filter((_, i) => i !== index);
	}
</script>

<form
	onsubmit={save}
	class="space-y-4 border-t border-hairline pt-4"
	aria-label="Named volume draft"
>
	<p id="volume-activation-note" class="text-sm text-text-muted">
		New volumes are saved as disabled drafts. Activation is unavailable in this build. Saving this
		draft does not move files or change the active storage destinations.
	</p>
	<fieldset disabled={saving} class="space-y-4">
		{#each draft.volumes as _, index (index)}<VolumeFields
				bind:value={draft.volumes[index]}
				{index}
				onremove={() => removeVolume(index)}
			/>{/each}
		<Button
			type="button"
			variant="outline"
			disabled={draft.volumes.length >= 32}
			onclick={() => {
				draft.volumes = [...draft.volumes, newVolume()];
			}}>Add volume</Button
		>
		<h4 class="text-sm font-medium">Placement policies</h4>
		<p class="text-xs text-text-muted">
			Changing a default only affects future placement. Existing files keep their catalog location.
			Metadata migration and bulk drain are not available here.
		</p>
		{#each draft.placement as _, index (index)}<PlacementFields
				bind:value={draft.placement[index]}
				{index}
				onremove={() => {
					draft.placement = draft.placement.filter((_, i) => i !== index);
				}}
			/>{/each}
		<Button
			type="button"
			variant="outline"
			disabled={draft.placement.length >= 256}
			onclick={() => {
				draft.placement = [...draft.placement, newPlacement()];
			}}>Add placement rule</Button
		>
	</fieldset>
	{#if validation}<p role="status" class="text-sm text-text-muted">{validation}</p>{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	<div class="flex flex-wrap gap-2">
		<Button type="submit" disabled={saving || !dirty || !!validation}
			>{saving ? 'Saving draft…' : 'Save volume draft'}</Button
		>
		<Button type="button" variant="outline" disabled={saving} onclick={cancel}>Cancel draft</Button>
	</div>
</form>
