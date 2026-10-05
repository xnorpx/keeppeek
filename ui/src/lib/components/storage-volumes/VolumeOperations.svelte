<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { onDestroy } from 'svelte';
	import {
		StorageVolumeCommandSchema,
		StorageObjectKind,
		StorageVolumeRole,
		type StorageObject,
		type StorageLegacyObject,
		type StorageMovePreview
	} from '$lib/proto/webrtc_pb';
	import type { VolumeController } from '$lib/storage-volumes';
	import { Button } from '$lib/components/ui/button/index.js';
	let { controller, volumes }: { controller: VolumeController; volumes: string[] } = $props();
	let source = $state('');
	let destination = $state('');
	const legacySource = 'legacy:recordings';
	type ListedObject = Pick<StorageLegacyObject, 'object' | 'bytes'>;
	let objects = $state<ListedObject[]>([]);
	let selected = $state<ListedObject | undefined>();
	let next = $state<StorageObject | undefined>();
	let role = $state(StorageVolumeRole.ARCHIVE);
	let preview = $state<StorageMovePreview | null>(null);
	let busy = $state(false);
	let loaded = $state(false);
	let error = $state<string | null>(null);
	let message = $state('');
	let alive = true;
	onDestroy(() => {
		alive = false;
	});
	function reset() {
		objects = [];
		selected = undefined;
		next = undefined;
		preview = null;
		loaded = false;
		message = '';
	}
	async function load(after?: StorageObject) {
		busy = true;
		error = null;
		preview = null;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, {
					action:
						source === legacySource
							? { case: 'legacyObjects', value: { after } }
							: { case: 'objects', value: { volumeId: source, after } }
				})
			);
			if (!alive) return;
			if (response.result.case !== 'objects' && response.result.case !== 'legacyObjects')
				throw new Error('Unexpected object list response.');
			objects = response.result.value.objects;
			next = response.result.value.nextAfter;
			selected = undefined;
			loaded = true;
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Objects unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function prepare() {
		if (!selected?.object) return;
		busy = true;
		error = null;
		preview = null;
		message = '';
		const kind = selected.object.kind;
		const selectedRole =
			kind === StorageObjectKind.EXPORT
				? StorageVolumeRole.EXPORT
				: kind === StorageObjectKind.THUMBNAIL
					? StorageVolumeRole.THUMBNAIL
					: role;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, {
					action: {
						case: 'previewMove',
						value: { object: selected.object, destinationVolumeId: destination, role: selectedRole }
					}
				})
			);
			if (!alive) return;
			if (response.result.case !== 'preview') throw new Error('Unexpected move preview response.');
			preview = response.result.value;
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Move preview unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function confirm() {
		if (!preview) return;
		busy = true;
		error = null;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, {
					action: {
						case: 'confirmMove',
						value: {
							previewToken: preview.previewToken,
							expectedConfigurationRevision: preview.configurationRevision
						}
					}
				})
			);
			if (!alive) return;
			if (response.result.case !== 'job') throw new Error('Unexpected move confirmation response.');
			message = `Move ${response.result.value.jobId}: ${response.result.value.phase}. Refresh move jobs to follow progress.`;
			preview = null;
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Move confirmation unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
</script>

<section class="space-y-3 border-t border-hairline pt-4" aria-label="Move one stored object">
	<h4 class="text-sm font-medium">Move one stored object</h4>
	<p class="text-xs text-text-muted">
		Preview checks the current source and destination. Confirm queues a durable move; readers finish
		before the old copy is removed.
	</p>
	<fieldset disabled={busy} class="space-y-3">
		<label
			>Source volume<select bind:value={source} onchange={reset}
				><option value="">Choose source volume</option><option value={legacySource}
					>Legacy recordings</option
				><option value="legacy-active">Managed files in original active storage</option>
				<option value="legacy-archive">Managed files in original archive storage</option>
				{#each volumes as id (id)}<option value={id}>{id}</option>{/each}</select
			></label
		>
		<Button type="button" variant="outline" disabled={!source} onclick={() => load()}
			>Load stored objects</Button
		>
		{#if loaded && objects.length === 0}<p class="text-sm text-text-muted">
				No eligible objects on this page.
			</p>{/if}
		{#if objects.length}<label
				>Stored object<select bind:value={selected} onchange={() => (preview = null)}
					><option value={undefined}>Choose object</option
					>{#each objects as object (`${object.object?.kind}:${object.object?.id}`)}<option
							value={object}
							>{object.object?.id} · {object.bytes === undefined
								? 'verify size in preview'
								: `${object.bytes.toString()} bytes`}</option
						>{/each}</select
				></label
			>{/if}
		{#if next}<Button type="button" variant="outline" onclick={() => load(next)}
				>Next stored objects</Button
			>{/if}
		<label
			>Destination volume<select bind:value={destination} onchange={() => (preview = null)}
				><option value="">Choose destination volume</option
				>{#each volumes.filter((id) => id !== source) as id (id)}<option value={id}>{id}</option
					>{/each}</select
			></label
		>
		{#if selected?.object?.kind === StorageObjectKind.RECORDING}<label
				>Destination role<select bind:value={role} onchange={() => (preview = null)}
					><option value={StorageVolumeRole.ARCHIVE}>Archive</option><option
						value={StorageVolumeRole.ACTIVE}>Active recordings</option
					></select
				></label
			>{/if}
		<Button
			type="button"
			variant="outline"
			disabled={!selected || !destination || destination === source}
			onclick={prepare}>Preview move</Button
		>
		{#if preview}
			<div class="rounded-sm border border-activity p-3 text-sm">
				<p class="break-all">
					{preview.source?.object?.id}: {preview.source?.volumeId} → {preview.destinationVolumeId}
				</p>
				<p>
					{preview.source?.bytes.toString()} bytes. Preview valid for {preview.expiresInSeconds} seconds;
					changed settings require a new preview.
				</p>
				{#if preview.adoptsLegacy}<p>
						Confirmation adopts this file into managed storage. Cancelling the transfer keeps it
						managed at its current location.
					</p>{/if}
				<Button type="button" onclick={confirm}>Confirm this move</Button>
			</div>
		{/if}
	</fieldset>
	{#if message}<p role="status" class="text-sm break-all">{message}</p>{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
</section>

<style>
	label {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
		font-size: 0.75rem;
	}
	select {
		width: 100%;
		min-width: 0;
		border: 1px solid var(--color-hairline);
		background: var(--color-surface);
		color: inherit;
		border-radius: 0.125rem;
		padding: 0.5rem;
		font-size: 0.875rem;
	}
	select:focus-visible {
		outline: 2px solid var(--color-activity);
		outline-offset: 2px;
	}
</style>
