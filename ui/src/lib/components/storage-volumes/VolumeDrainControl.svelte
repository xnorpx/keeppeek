<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { onDestroy } from 'svelte';
	import {
		StorageVolumeCommandSchema,
		type StorageVolumeList,
		type StorageVolumeStatus
	} from '$lib/proto/webrtc_pb';
	import type { VolumeController } from '$lib/storage-volumes';
	import { Button } from '$lib/components/ui/button/index.js';
	type Props = {
		volume: StorageVolumeStatus;
		revision: string;
		controller: VolumeController;
		onchange: (status: StorageVolumeList) => void;
		disabled: boolean;
	};
	let { volume, revision, controller, onchange, disabled }: Props = $props();
	let busy = $state(false);
	let error = $state<string | null>(null);
	let alive = true;
	onDestroy(() => {
		alive = false;
	});
	async function toggle() {
		const draining = !volume.operatorDraining;
		const question = draining
			? `Stop new writes to ${volume.volumeId}? Existing writes will finish. This does not move stored files.`
			: `Clear operator drain for ${volume.volumeId}? Saved configuration will still control whether new writes are allowed.`;
		if (!window.confirm(question)) return;
		busy = true;
		error = null;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, {
					action: {
						case: 'setDraining',
						value: { volumeId: volume.volumeId, draining, expectedConfigurationRevision: revision }
					}
				})
			);
			if (!alive) return;
			if (response.result.case !== 'volumes')
				throw new Error('Unexpected drain response. Refresh volume status.');
			onchange(response.result.value);
		} catch (cause) {
			if (alive)
				error =
					cause instanceof Error
						? cause.message
						: 'Drain update failed. Refresh volume status and retry.';
		} finally {
			if (alive) busy = false;
		}
	}
</script>

<div class="space-y-1">
	<Button
		type="button"
		size="sm"
		variant="outline"
		disabled={disabled || busy}
		onclick={toggle}
		aria-label={volume.operatorDraining
			? `Clear operator drain for ${volume.volumeId}`
			: `Stop new writes to ${volume.volumeId}`}
	>
		{volume.operatorDraining ? 'Clear operator drain' : 'Stop new writes'}
	</Button>
	{#if busy}<p role="status" class="text-xs text-text-muted">Updating drain…</p>{/if}
	{#if error}<p role="alert" class="text-xs text-destructive">{error}</p>{/if}
</div>
