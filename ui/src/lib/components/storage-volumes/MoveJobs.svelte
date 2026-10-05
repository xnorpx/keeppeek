<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { StorageVolumeCommandSchema, type StorageMoveJob } from '$lib/proto/webrtc_pb';
	import type { VolumeController } from '$lib/storage-volumes';
	import { Button } from '$lib/components/ui/button/index.js';
	import { onDestroy } from 'svelte';
	let { controller }: { controller: VolumeController } = $props();
	let jobs = $state<StorageMoveJob[]>([]);
	let next = $state('');
	let loaded = $state(false);
	let busy = $state(false);
	let error = $state<string | null>(null);
	let alive = true;
	onDestroy(() => {
		alive = false;
	});
	async function refresh(after = '') {
		busy = true;
		error = null;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, {
					action: { case: 'moves', value: { afterJobId: after } }
				})
			);
			if (!alive) return;
			if (response.result.case !== 'jobs') throw new Error('Unexpected move status response.');
			jobs = response.result.value.jobs;
			next = response.result.value.nextAfterJobId;
			loaded = true;
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Move status unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function cancel(jobId: string) {
		busy = true;
		error = null;
		try {
			const response = await controller.storageVolumes(
				create(StorageVolumeCommandSchema, { action: { case: 'cancelMove', value: { jobId } } })
			);
			if (!alive) return;
			if (response.result.case !== 'job') throw new Error('Unexpected move cancellation response.');
			const changed = response.result.value;
			jobs = jobs.map((job) => (job.jobId === changed.jobId ? changed : job));
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Cancellation unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
</script>

<section
	class="space-y-3 border-t border-hairline pt-4"
	aria-label="Storage move jobs"
	aria-busy={busy}
>
	<div class="flex flex-wrap items-center justify-between gap-2">
		<h4 class="text-sm font-medium">Move jobs</h4>
		<Button type="button" size="sm" variant="outline" disabled={busy} onclick={() => refresh()}
			>Refresh move jobs</Button
		>
	</div>
	{#if loaded && jobs.length === 0}<p class="text-sm text-text-muted">
			No move jobs on this page.
		</p>{/if}
	<ul class="space-y-2">
		{#each jobs as job (job.jobId)}
			<li class="rounded-sm border border-hairline p-3 text-sm">
				<p class="font-mono text-xs break-all">{job.jobId}</p>
				<p>
					{job.source?.volumeId} → {job.destinationVolumeId} · {job.phase}{job.cancellationRequested
						? ' · Cancellation requested'
						: ''}
				</p>
				{#if ['reserved', 'verified', 'file_published'].includes(job.phase)}<Button
						type="button"
						size="sm"
						variant="outline"
						disabled={busy || job.cancellationRequested}
						onclick={() => cancel(job.jobId)}>Cancel move {job.jobId}</Button
					>{/if}
			</li>
		{/each}
	</ul>
	{#if next}<Button type="button" variant="outline" disabled={busy} onclick={() => refresh(next)}
			>Next move jobs</Button
		>{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
</section>
