<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { onDestroy } from 'svelte';
	import {
		StorageVolumeCommandSchema,
		StorageObjectKind,
		StorageVolumeRole,
		type StorageObject,
		type StorageObjectLocation,
		type StorageMovePreview
	} from '$lib/proto/webrtc_pb';
	import type { VolumeController } from '$lib/storage-volumes';
	import { Button } from '$lib/components/ui/button/index.js';
	let { controller, volumes }: { controller: VolumeController; volumes: string[] } = $props();
	let source = $state('');
	let destination = $state('');
	let objects = $state<StorageObjectLocation[]>([]);
	let selected = $state<StorageObjectLocation | undefined>();
	let next = $state<StorageObject | undefined>();
	let role = $state(StorageVolumeRole.ARCHIVE);
	let preview = $state<StorageMovePreview | null>(null);
	let busy = $state(false);
	let batch = $state<StorageMovePreview[]>([]);
	let batchMessages = $state<string[]>([]);
	let stopBatch = $state(false);
	let batchBytes = $derived(batch.reduce((sum, item) => sum + (item.source?.bytes ?? 0n), 0n));
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
		batch = [];
		batchMessages = [];
		loaded = false;
		message = '';
	}
	async function load(after?: StorageObject) {
		busy = true;
		error = null;
		preview = null;
		batch = [];
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, {
					action: { case: 'objects', value: { volumeId: source, after } }
				})
			);
			if (!alive) return;
			if (response.result.case !== 'objects') throw new Error('Unexpected object list response.');
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
	async function previewObject(object: StorageObject): Promise<StorageMovePreview> {
		const selectedRole =
			object.kind === StorageObjectKind.EXPORT
				? StorageVolumeRole.EXPORT
				: object.kind === StorageObjectKind.THUMBNAIL
					? StorageVolumeRole.THUMBNAIL
					: role;
		const response = await controller.storageVolumes(
			create(StorageVolumeCommandSchema, {
				action: {
					case: 'previewMove',
					value: { object, destinationVolumeId: destination, role: selectedRole }
				}
			})
		);
		if (response.result.case !== 'preview') throw new Error('Unexpected move preview response.');
		return response.result.value;
	}
	async function prepareBatch() {
		busy = true;
		stopBatch = false;
		batch = [];
		batchMessages = [];
		preview = null;
		error = null;
		// ponytail: one page batch of 16 reuses individual durable jobs; no second job system.
		try {
			for (const item of objects.slice(0, 16)) {
				if (!alive || stopBatch) break;
				if (!item.object) continue;
				try {
					const plan = await previewObject(item.object);
					if (alive) batch = [...batch, plan];
				} catch (cause) {
					if (alive)
						batchMessages = [
							...batchMessages.slice(-63),
							`${item.object.id}: ${cause instanceof Error ? cause.message : 'Preview failed'}`
						];
				}
			}
		} finally {
			if (alive) busy = false;
		}
	}
	async function confirmPlan(plan: StorageMovePreview) {
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, {
					action: {
						case: 'confirmMove',
						value: {
							previewToken: plan.previewToken,
							expectedConfigurationRevision: plan.configurationRevision
						}
					}
				})
			);
			if (response.result.case !== 'job') throw new Error('Unexpected move confirmation response.');
			return response.result.value;
		} catch (cause) {
			// A lost confirmation reply must not cause a second move with a new token.
			if (!alive || stopBatch) throw cause;
			const status = await controller
				.storageVolumes(
					create(StorageVolumeCommandSchema, {
						action: { case: 'getMove', value: { jobId: plan.jobId } }
					})
				)
				.catch(() => null);
			if (status?.result.case === 'job') return status.result.value;
			throw cause;
		}
	}
	async function confirmBatch() {
		selected = undefined;
		busy = true;
		stopBatch = false;
		try {
			for (const plan of [...batch]) {
				if (!alive || stopBatch) break;
				try {
					const job = await confirmPlan(plan);
					if (!alive) break;
					batch = batch.filter((item) => item.previewToken !== plan.previewToken);
					objects = objects.filter(
						(item) =>
							item.object?.kind !== plan.source?.object?.kind ||
							item.object?.id !== plan.source?.object?.id
					);
					batchMessages = [
						...batchMessages.slice(-63),
						`${job.jobId}: ${job.phase}. Refresh move jobs for progress or cancellation.`
					];
				} catch (cause) {
					if (alive)
						batchMessages = [
							...batchMessages.slice(-63),
							`${plan.source?.object?.id}: ${cause instanceof Error ? cause.message : 'Confirmation outcome unknown'}. Check move jobs before previewing again.`
						];
					break;
				}
			}
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
		batch = [];
		try {
			const plan = await previewObject(selected.object);
			if (alive) preview = plan;
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
		stopBatch = false;
		try {
			const job = await confirmPlan(preview);
			if (!alive) return;
			message = `Move ${job.jobId}: ${job.phase}. Refresh move jobs to follow progress.`;
			preview = null;
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Move confirmation unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
</script>

<section
	class="space-y-3 border-t border-hairline pt-4"
	aria-label="Move stored objects"
	aria-busy={busy}
>
	<h4 class="text-sm font-medium">Move stored objects</h4>
	<p class="text-xs text-text-muted">
		Preview checks the current source and destination. Confirm queues a durable move; readers finish
		before the old copy is removed. Stop new writes to drain a volume, then move its objects in
		batches. Metadata uses the separate catalog and export history controls.
	</p>
	<fieldset disabled={busy} class="space-y-3">
		<label
			>Source volume<select bind:value={source} onchange={reset}
				><option value="">Choose source volume</option>{#each volumes as id (id)}<option value={id}
						>{id}</option
					>{/each}</select
			></label
		>
		<Button type="button" variant="outline" disabled={!source} onclick={() => load()}
			>Load stored objects</Button
		>
		{#if loaded && objects.length === 0}<p class="text-sm text-text-muted">
				No owned objects on this page.
			</p>{/if}
		{#if objects.length}<label
				>Stored object<select
					bind:value={selected}
					onchange={() => {
						preview = null;
						batch = [];
					}}
					><option value={undefined}>Choose object</option
					>{#each objects as object (`${object.object?.kind}:${object.object?.id}`)}<option
							value={object}>{object.object?.id} · {object.bytes.toString()} bytes</option
						>{/each}</select
				></label
			>{/if}
		{#if next}<Button type="button" variant="outline" onclick={() => load(next)}
				>Next stored objects</Button
			>{/if}
		<label
			>Destination volume<select
				bind:value={destination}
				onchange={() => {
					preview = null;
					batch = [];
				}}
				><option value="">Choose destination volume</option
				>{#each volumes.filter((id) => id !== source) as id (id)}<option value={id}>{id}</option
					>{/each}</select
			></label
		>
		{#if objects.some((item) => item.object?.kind === StorageObjectKind.RECORDING)}<label
				>Destination role<select
					bind:value={role}
					onchange={() => {
						preview = null;
						batch = [];
					}}
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
		<Button
			type="button"
			variant="outline"
			disabled={!objects.length || !destination || destination === source}
			onclick={prepareBatch}
		>
			Preview next {Math.min(objects.length, 16)} objects
		</Button>
		{#if batch.length}
			<div class="rounded-sm border border-activity p-3 text-sm" role="status">
				<p>
					{batch.length} files, {batchBytes.toString()} bytes to {destination}. Duration depends on
					the disks; no time estimate is available.
				</p>
				<p>Only these previews will be queued. Changed or expired previews require review again.</p>
				<Button type="button" onclick={confirmBatch}>Confirm batch of {batch.length} moves</Button>
			</div>
		{/if}
		{#if preview}
			<div class="rounded-sm border border-activity p-3 text-sm">
				<p class="break-all">
					{preview.source?.object?.id}: {preview.source?.volumeId} → {preview.destinationVolumeId}
				</p>
				<p>
					{preview.source?.bytes.toString()} bytes. Preview valid for {preview.expiresInSeconds} seconds;
					changed settings require a new preview.
				</p>
				<Button type="button" onclick={confirm}>Confirm this move</Button>
			</div>
		{/if}
	</fieldset>
	{#if busy}<Button type="button" variant="outline" onclick={() => (stopBatch = true)}
			>Stop after current request</Button
		>{/if}
	{#if batchMessages.length}<ul aria-label="Batch move results" class="space-y-1 text-sm break-all">
			{#each batchMessages as result, index (index)}<li>{result}</li>{/each}
		</ul>{/if}
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
