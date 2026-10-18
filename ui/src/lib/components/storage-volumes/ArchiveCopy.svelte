<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { onDestroy } from 'svelte';
	import { archiveJobId, submitArchiveCopy, type ArchiveController } from '$lib/archive';
	import {
		StorageVolumeCommandSchema,
		StorageObjectKind,
		type StorageObject,
		type StorageObjectLocation,
		type ArchiveJob
	} from '$lib/proto/webrtc_pb';
	import { Button } from '$lib/components/ui/button/index.js';
	let {
		controller,
		disabled = false,
		onjob
	}: {
		controller: ArchiveController;
		disabled?: boolean;
		onjob: (job: ArchiveJob) => void;
	} = $props();
	let volumes = $state<string[]>([]);
	let source = $state('');
	let objects = $state<StorageObjectLocation[]>([]);
	let next = $state<StorageObject | undefined>();
	let selectedId = $state('');
	let selected = $derived(
		objects.find((item) => `${item.object?.kind}:${item.object?.id}` === selectedId)?.object
	);
	let intent: { key: string; jobId: string } | null = null;
	let busy = $state(false);
	let error = $state('');
	let loaded = $state(false);
	let alive = true;
	onDestroy(() => {
		alive = false;
	});
	async function loadVolumes() {
		busy = true;
		error = '';
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, { action: { case: 'list', value: {} } })
			);
			if (!alive) return;
			if (response.result.case !== 'volumes') throw new Error('Unexpected volume list response.');
			volumes = response.result.value.volumes.map((volume) => volume.volumeId);
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Source volumes unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function load(after?: StorageObject) {
		busy = true;
		error = '';
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, {
					action: { case: 'objects', value: { volumeId: source, after } }
				})
			);
			if (!alive) return;
			if (response.result.case !== 'objects') throw new Error('Unexpected media list response.');
			objects = response.result.value.objects.filter(
				(item) =>
					item.object?.kind === StorageObjectKind.RECORDING ||
					item.object?.kind === StorageObjectKind.EXPORT
			);
			next = response.result.value.nextAfter;
			selectedId = '';
			loaded = true;
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Media unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function copy() {
		if (!selected) return;
		const key = `${selected.kind}:${selected.id}`;
		busy = true;
		error = '';
		try {
			if (!intent || intent.key !== key) intent = { key, jobId: archiveJobId() };
			const job = await submitArchiveCopy(controller, selected, intent.jobId);
			if (alive) onjob(job);
		} catch (cause) {
			if (alive) {
				const message = cause instanceof Error ? cause.message : 'Copy outcome unknown';
				error =
					intent?.key === key
						? `${message}. Job ${intent.jobId}. Retrying this selection reuses the same job identity.`
						: `${message} No archive copy was queued.`;
			}
		} finally {
			if (alive) busy = false;
		}
	}
</script>

<section class="space-y-3 border-t border-hairline pt-4" aria-label="Copy media to archive">
	<h4 class="text-sm font-medium">Copy media to archive</h4>
	<p class="text-xs text-text-muted">
		Only finalized recordings and completed exports are eligible. Copy queues bounded background
		work; recording continues. Refresh archive jobs for progress.
	</p>
	<fieldset disabled={disabled || busy} class="flex flex-wrap items-end gap-3">
		<Button class="min-h-11 sm:min-h-0" type="button" variant="outline" onclick={loadVolumes}
			>Load archive source volumes</Button
		>
		<label class="text-sm"
			>Source volume<select
				aria-label="Source volume"
				class="block min-h-11 max-w-full rounded-sm border border-hairline bg-surface p-2 sm:min-h-0"
				bind:value={source}
				onchange={() => {
					objects = [];
					selectedId = '';
					next = undefined;
					loaded = false;
				}}
				><option value="">Select a volume</option>{#each volumes as volume (volume)}<option
						value={volume}>{volume}</option
					>{/each}</select
			></label
		>
		<Button
			class="min-h-11 sm:min-h-0"
			type="button"
			variant="outline"
			disabled={!source}
			onclick={() => load()}>Load eligible media</Button
		>
		{#if objects.length}<label class="max-w-full min-w-0 text-sm"
				>Media object<select
					aria-label="Media object"
					class="block min-h-11 w-full max-w-full rounded-sm border border-hairline bg-surface p-2 sm:min-h-0"
					bind:value={selectedId}
					><option value="">Select media</option
					>{#each objects as item (`${item.object?.kind}:${item.object?.id}`)}<option
							value={`${item.object?.kind}:${item.object?.id}`}
							>{item.object?.kind === StorageObjectKind.EXPORT ? 'Export' : 'Recording'} · {item
								.object?.id} · {item.bytes.toString()} bytes</option
						>{/each}</select
				></label
			>{/if}
		<Button class="min-h-11 sm:min-h-0" type="button" disabled={!selected} onclick={copy}
			>Queue archive copy</Button
		>
		{#if next}<Button
				class="min-h-11 sm:min-h-0"
				type="button"
				variant="outline"
				onclick={() => load(next)}>Next media page</Button
			>{/if}
	</fieldset>
	{#if loaded && !objects.length}<p class="text-sm text-text-muted">
			No recordings or exports on this page.
		</p>{/if}
	{#if error}<p role="alert" class="text-sm break-words text-destructive">{error}</p>{/if}
</section>
