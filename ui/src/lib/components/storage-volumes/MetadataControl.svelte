<script lang="ts">
	import { create, type MessageInitShape } from '@bufbuild/protobuf';
	import { onMount } from 'svelte';
	import {
		StorageVolumeCommandSchema,
		type StorageMetadataPreview,
		type StorageMetadataStatus
	} from '$lib/proto/webrtc_pb';
	import type { VolumeController } from '$lib/storage-volumes';
	import { Button } from '$lib/components/ui/button/index.js';
	let {
		controller,
		volumes,
		disabled = false,
		onpendingchange
	}: {
		controller: VolumeController;
		volumes: string[];
		disabled?: boolean;
		onpendingchange?: (pending: boolean) => void;
	} = $props();
	let status = $state<StorageMetadataStatus | null>(null);
	let preview = $state<StorageMetadataPreview | null>(null);
	let destination = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);
	let alive = true;
	onMount(() => {
		void refresh();
		return () => {
			alive = false;
		};
	});
	async function send(action: MessageInitShape<typeof StorageVolumeCommandSchema>['action']) {
		busy = true;
		error = null;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, { action })
			);
			if (!alive) return;
			if (response.result.case === 'metadata') {
				status = response.result.value;
				onpendingchange?.(status.restartRequired);
				preview = null;
			} else if (response.result.case === 'metadataPreview') {
				preview = response.result.value;
			} else throw new Error('Unexpected metadata response. Refresh status and retry.');
		} catch (cause) {
			if (alive) {
				preview = null;
				error =
					cause instanceof Error
						? cause.message
						: 'Metadata operation failed. Refresh status and retry.';
			}
		} finally {
			if (alive) busy = false;
		}
	}
	function refresh() {
		preview = null;
		return send({ case: 'metadata', value: {} });
	}
	function prepare() {
		if (disabled || busy || !destination || status?.pendingVolumeId) return;
		preview = null;
		return send({ case: 'previewMetadata', value: { destinationVolumeId: destination } });
	}
	function confirm() {
		if (disabled || busy || !preview) return;
		return send({
			case: 'confirmMetadata',
			value: {
				previewToken: preview.previewToken,
				expectedConfigurationRevision: preview.configurationRevision
			}
		});
	}
	function cancel() {
		if (disabled || busy || !status?.pendingVolumeId) return;
		return send({
			case: 'cancelMetadata',
			value: { expectedConfigurationRevision: status.configurationRevision }
		});
	}
</script>

<section
	class="min-w-0 space-y-3 rounded-sm border border-hairline p-3"
	aria-labelledby="metadata-heading"
>
	<div class="flex flex-wrap items-center justify-between gap-2">
		<h4 id="metadata-heading" class="font-semibold">Catalog and export history</h4>
		<Button type="button" variant="outline" size="sm" disabled={busy} onclick={refresh}
			>Refresh metadata status</Button
		>
	</div>
	<p class="text-sm text-text-muted">
		Move the catalog and export history together. Recordings, exports and thumbnails keep their
		current locations.
	</p>
	{#if status}
		<p class="text-sm break-all">
			Current metadata location: {status.currentVolumeId ?? 'Legacy paths'}
		</p>
		{#if status.pendingVolumeId}
			<p role="status" class="text-sm break-all">
				Metadata move pending: {status.pendingVolumeId}. Restart the service to apply it. Storage
				settings are locked until this completes or is cancelled.
			</p>
			<Button type="button" variant="outline" size="sm" disabled={disabled || busy} onclick={cancel}
				>Cancel pending metadata move</Button
			>
		{:else}
			<label class="block space-y-1 text-sm">
				<span>Metadata destination</span>
				<select
					class="w-full min-w-0 rounded-sm border border-hairline bg-surface p-2"
					bind:value={destination}
					disabled={disabled || busy}
					onchange={() => (preview = null)}
				>
					<option value="">Choose a disabled metadata volume</option>
					{#each volumes as volume (volume)}<option value={volume}>{volume}</option>{/each}
				</select>
			</label>
			{#if !volumes.length}<p class="text-sm text-text-muted">
					Add and save a disabled volume with only the Metadata role to choose a destination.
				</p>{/if}
			<Button
				type="button"
				variant="outline"
				size="sm"
				disabled={disabled || busy || !destination}
				onclick={prepare}>Preview metadata move</Button
			>
		{/if}
	{/if}
	{#if preview}
		<div class="space-y-2 rounded-sm border border-hairline p-3 text-sm">
			<p class="break-all">Destination: {preview.destinationVolumeId}</p>
			<p class="break-words">Required space: {preview.requiredBytes.toString()} bytes.</p>
			<p>
				Requires a service restart and downtime while the catalog and history are copied. The
				previous files are retained. Confirming schedules the move; it does not restart the service.
			</p>
			<Button type="button" size="sm" disabled={disabled || busy} onclick={confirm}
				>Confirm metadata move</Button
			>
		</div>
	{/if}
	{#if busy}<p role="status" class="text-sm text-text-muted">Checking metadata…</p>{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
</section>
