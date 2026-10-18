<script lang="ts">
	import { untrack } from 'svelte';
	import { beforeNavigate } from '$app/navigation';
	import {
		archiveDraft,
		archiveDraftError,
		archiveNumberFields,
		encodeArchiveDraft,
		type ArchiveController
	} from '$lib/archive';
	import type { SanitizedConfig, SettingsConfigUpdateResponse } from '$lib/types';
	import { Button } from '$lib/components/ui/button/index.js';
	let {
		config,
		controller,
		onsaved,
		oncancel
	}: {
		config: SanitizedConfig;
		controller: ArchiveController;
		onsaved: (result: SettingsConfigUpdateResponse) => void;
		oncancel: () => void;
	} = $props();
	let snapshot = $state.raw(untrack(() => config));
	let initial = $state(archiveDraft(untrack(() => snapshot.storage.archive)));
	let draft = $state(archiveDraft(untrack(() => snapshot.storage.archive)));
	let dirty = $derived(JSON.stringify(draft) !== JSON.stringify(initial));
	let validation = $derived(archiveDraftError(draft));
	let saving = $state(false);
	let error = $state<string | null>(null);
	let accepted = false;
	beforeNavigate(({ cancel }) => {
		if (!accepted && dirty && !window.confirm('Discard your unsaved archive settings?')) cancel();
	});
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
				storage: { ...snapshot.storage, archive: encodeArchiveDraft(draft) }
			});
			accepted = true;
			onsaved(result);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Archive settings were not saved.';
		} finally {
			saving = false;
		}
	}
	async function reload() {
		if (
			dirty &&
			!window.confirm('Discard your unsaved archive settings and reload the current configuration?')
		)
			return;
		saving = true;
		error = null;
		try {
			snapshot = await controller.getRuntimeConfiguration();
			initial = archiveDraft(snapshot.storage.archive);
			draft = archiveDraft(snapshot.storage.archive);
		} catch (cause) {
			error =
				cause instanceof Error ? cause.message : 'Current archive settings could not be loaded.';
		} finally {
			saving = false;
		}
	}
</script>

<form
	onsubmit={save}
	aria-label="Local archive settings"
	class="space-y-4 border-t border-hairline pt-4"
>
	<p class="text-sm text-text-muted">
		Keep verified copies of finalized recordings and completed exports. Review them in Keep; archive
		playback and seeking may take longer.
	</p>
	<fieldset disabled={saving} class="grid gap-3 sm:grid-cols-2">
		<label class="space-y-2 text-sm font-medium sm:col-span-2"
			>Archive folder<input
				aria-label="Archive folder"
				class="h-11 w-full rounded-sm border border-hairline-strong bg-raised px-3 font-mono"
				bind:value={draft.root}
				maxlength="4096"
			/></label
		>
		<p class="text-sm text-text-muted sm:col-span-2">
			Use an existing folder on another disk. A disconnected or replaced disk pauses archive work.
		</p>
		<label class="space-y-2 text-sm"
			>Target ID<input
				aria-label="Target ID"
				class="h-11 w-full rounded-sm border border-hairline-strong bg-raised px-3 font-mono"
				bind:value={draft.targetId}
				maxlength="256"
			/></label
		>
		<p class="text-sm text-text-muted sm:self-end">
			This name stays attached to verified copies. Use a new ID when replacing the archive
			destination.
		</p>
		<label class="flex min-h-11 items-center gap-2 text-sm sm:min-h-0"
			><input type="checkbox" bind:checked={draft.enabled} />Enable local archive</label
		>
		<p class="text-xs text-text-muted sm:col-span-2">
			Automatic copying and primary eviction are off by default. Choose sources and a minimum age in
			advanced settings.
		</p>
		<label class="flex min-h-11 items-center gap-2 text-sm sm:min-h-0"
			><input type="checkbox" bind:checked={draft.automatic} />Automatically archive eligible media</label
		>
		<label class="flex min-h-11 items-center gap-2 text-sm sm:col-span-2 sm:min-h-0"
			><input type="checkbox" bind:checked={draft.evictAfterArchive} />Allow automatic eviction
			after verified archive copy</label
		>
		<p class="text-xs text-text-muted sm:col-span-2">
			Eviction removes the primary file after verification and reader checks. Timeline metadata
			remains available for archive playback. Leave this off to retain primary copies.
		</p>
		<details class="space-y-3 border-t border-hairline pt-4 sm:col-span-2">
			<summary
				class="min-h-11 cursor-pointer py-3 text-sm font-medium focus-visible:outline-2 focus-visible:outline-ring sm:min-h-0 sm:py-0"
				>Advanced selection &amp; transfer limits</summary
			>
			<div class="grid gap-3 pt-3 sm:grid-cols-2">
				<label class="space-y-1 text-sm sm:col-span-2"
					>Automatic source IDs (one per line; empty selects all)<textarea
						class="w-full rounded-sm border border-hairline bg-surface px-3 py-2"
						rows="3"
						bind:value={draft.sourceIds}
						maxlength="33000"></textarea></label
				>
				{#each archiveNumberFields as field (field.key)}
					<label class="space-y-1 text-sm"
						>{field.label}<input
							aria-label={field.label}
							class="min-h-11 w-full rounded-sm border border-hairline bg-surface px-3 py-2 sm:min-h-0"
							inputmode="numeric"
							bind:value={draft[field.key]}
							maxlength="19"
						/><span class="block text-xs text-text-muted"
							>{field.min.toString()}–{field.max.toString()}</span
						></label
					>
				{/each}
			</div>
		</details>
	</fieldset>
	<p class="text-sm text-text-muted">
		Save, restart, then test the destination before archiving. Current storage stays active until
		restart. Secret references remain in config.toml.
	</p>
	{#if validation}<p class="text-sm text-text-muted">{validation}</p>{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">
			{error} Your draft is preserved. Reload current settings before retrying a revision conflict.
		</p>{/if}
	<div class="flex flex-wrap gap-2">
		<Button class="min-h-11 sm:min-h-0" type="submit" disabled={saving || !dirty || !!validation}
			>{saving ? 'Saving archive settings…' : 'Save archive settings'}</Button
		>
		<Button
			class="min-h-11 sm:min-h-0"
			type="button"
			variant="outline"
			disabled={saving}
			onclick={reload}>Reload archive settings</Button
		>
		<Button
			class="min-h-11 sm:min-h-0"
			type="button"
			variant="outline"
			disabled={saving}
			onclick={() => {
				if (!dirty || window.confirm('Discard your unsaved archive settings?')) oncancel();
			}}>Cancel archive edit</Button
		>
	</div>
</form>
