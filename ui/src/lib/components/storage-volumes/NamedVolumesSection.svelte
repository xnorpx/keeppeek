<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { onMount } from 'svelte';
	import {
		StorageVolumeCommandSchema,
		StorageVolumeRole,
		StorageVolumeState,
		type StorageVolumeList
	} from '$lib/proto/webrtc_pb';
	import type { SanitizedConfig, SettingsConfigUpdateResponse } from '$lib/types';
	import type { VolumeController } from '$lib/storage-volumes';
	import { Button } from '$lib/components/ui/button/index.js';
	import VolumeDraftEditor from './VolumeDraftEditor.svelte';
	import VolumeOperations from './VolumeOperations.svelte';
	import MoveJobs from './MoveJobs.svelte';
	import MetadataControl from './MetadataControl.svelte';
	import VolumeDrainControl from './VolumeDrainControl.svelte';
	let {
		config,
		controller,
		onsaved,
		disabled = false,
		onpendingchange
	}: {
		config: SanitizedConfig;
		controller: VolumeController;
		onsaved: (result: SettingsConfigUpdateResponse) => void;
		disabled?: boolean;
		onpendingchange?: (pending: boolean) => void;
	} = $props();
	let metadataVolumes = $derived(
		(config.storage.named_volumes?.volumes ?? [])
			.filter(
				(volume) =>
					volume.state === StorageVolumeState.DISABLED &&
					volume.roles.length === 1 &&
					volume.roles[0] === StorageVolumeRole.METADATA &&
					!volume.sources.length &&
					!volume.groups.length
			)
			.map((volume) => volume.id)
	);
	let status = $state<StorageVolumeList | null>(null);
	let busy = $state(false);
	let editing = $state(false);
	let error = $state<string | null>(null);
	let message = $state('');
	let alive = true;
	onMount(() => {
		void refresh();
		return () => {
			alive = false;
		};
	});
	async function refresh() {
		busy = true;
		error = null;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, { action: { case: 'list', value: {} } })
			);
			if (!alive) return;
			if (response.result.case !== 'volumes') throw new Error('Unexpected volume status response.');
			status = response.result.value;
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Volume status unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function probe(volumeId: string) {
		busy = true;
		error = null;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, { action: { case: 'probe', value: { volumeId } } })
			);
			if (!alive) return;
			if (response.result.case !== 'probe') throw new Error('Unexpected volume probe response.');
			const checked = response.result.value;
			if (status)
				status = {
					...status,
					volumes: status.volumes.map((v) => (v.volumeId === checked.volumeId ? checked : v))
				};
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Volume probe unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	function saved(result: SettingsConfigUpdateResponse) {
		onsaved(result);
		editing = false;
		message = 'Volume settings saved. Restart to apply the configuration.';
		void refresh();
	}
</script>

<section
	class="mt-4 space-y-4 rounded-sm border border-hairline p-5"
	aria-labelledby="named-volume-heading"
>
	<div class="flex flex-wrap items-center justify-between gap-3">
		<h3 id="named-volume-heading" class="text-base font-semibold">Named storage volumes</h3>
		<div class="flex flex-wrap gap-2">
			<Button type="button" size="sm" variant="outline" disabled={busy} onclick={refresh}
				>Refresh volume status</Button
			><Button
				type="button"
				size="sm"
				variant="outline"
				disabled={disabled || editing}
				onclick={() => (editing = true)}>Edit volume draft</Button
			>
		</div>
	</div>
	<p class="text-sm text-text-muted">
		Keep separate destinations for recordings, exports and images. Drafts preserve secret references
		and exact byte limits. Availability probes inspect configured roots without creating
		directories.
	</p>
	{#if busy}<p role="status" class="text-sm text-text-muted">Checking storage…</p>{/if}
	{#if status}
		<p class="text-xs text-text-muted">
			{status.runtimeAvailable
				? 'Volume runtime is available.'
				: 'Volume runtime is unavailable. Disabled drafts can be edited; moves require an active runtime.'}
		</p>
		{#if !status.volumes.length}<p class="text-sm text-text-muted">
				No named volumes configured. Configure placement before recording media.
			</p>{/if}
		<ul class="space-y-2">
			{#each status.volumes as volume (volume.volumeId)}
				<li
					class="flex flex-wrap items-center justify-between gap-3 rounded-sm border border-hairline p-3"
				>
					<div class="min-w-0 text-sm">
						<p class="font-medium break-all">
							{volume.volumeId} · {volume.online ? 'Online' : 'Offline or unavailable'}
						</p>
						<p class="text-xs break-words text-text-muted">
							Owned: {volume.ownedBytes.toString()} bytes · Reserved: {volume.reservedBytes.toString()}
							bytes · Available: {volume.availableBytes?.toString() ??
								'Unknown'}{volume.availableBytes === undefined ? '' : ' bytes'}
						</p>
						{#if volume.configuredDraining}<p class="text-xs text-text-muted">
								Draining from saved configuration.
							</p>{/if}
						{#if volume.operatorDraining}<p class="text-xs text-text-muted">
								Operator drain is active.
							</p>{/if}
					</div>
					<VolumeDrainControl
						{volume}
						revision={status.configurationRevision}
						{controller}
						onchange={(updated) => (status = updated)}
						disabled={disabled || busy || editing || !status.runtimeAvailable}
					/>
					<Button
						type="button"
						size="sm"
						variant="outline"
						disabled={busy}
						aria-label={`Probe ${volume.volumeId}`}
						onclick={() => probe(volume.volumeId)}>Probe</Button
					>
				</li>
			{/each}
		</ul>
	{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	{#if message}<p role="status" class="text-sm">{message}</p>{/if}
	{#if editing}<VolumeDraftEditor
			{config}
			{controller}
			onsaved={saved}
			oncancel={() => (editing = false)}
		/>{/if}
	<MetadataControl
		{controller}
		{onpendingchange}
		volumes={metadataVolumes}
		disabled={disabled || editing}
	/>
	{#if status?.runtimeAvailable}
		<VolumeOperations {controller} volumes={status.volumes.map((v) => v.volumeId)} />
		<MoveJobs {controller} />
	{/if}
</section>
