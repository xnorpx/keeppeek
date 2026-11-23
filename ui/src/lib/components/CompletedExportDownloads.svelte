<script lang="ts">
	import { onMount } from 'svelte';
	import type { ControlClient, MediaExportJob } from '$lib/control-client';
	import { Button } from '$lib/components/ui/button/index.js';
	import { saveExportDownload } from '$lib/save-export-download';

	let {
		controller
	}: { controller: Pick<ControlClient, 'onAccessState' | 'listExports' | 'downloadExport'> } =
		$props();
	let session = $state.raw<{ id: string; generation: number } | null>(null);
	let jobs = $state.raw<MediaExportJob[] | null>(null);
	let activity = $state<'finding' | 'downloading' | null>(null);
	let error = $state('');

	onMount(() => {
		const stop = controller.onAccessState((access) => {
			const next = access.status === 'authenticated' ? access.session : null;
			if (next && next.id === session?.id && access.generation === session.generation) return;
			session = next ? { id: next.id, generation: access.generation } : null;
			jobs = null;
			activity = null;
			error = '';
		});
		return () => {
			stop();
			session = null;
		};
	});

	async function find(): Promise<void> {
		const owner = session;
		if (!owner || activity) return;
		activity = 'finding';
		error = '';
		try {
			const results = await controller.listExports();
			if (owner === session) jobs = results.filter((item) => item.status === 'ready');
		} catch (cause) {
			if (owner === session)
				error = cause instanceof Error ? cause.message : 'Completed exports could not be loaded.';
		} finally {
			if (owner === session) activity = null;
		}
	}

	async function download(item: MediaExportJob): Promise<void> {
		const owner = session;
		if (!owner || activity) return;
		activity = 'downloading';
		error = '';
		try {
			const result = await controller.downloadExport(item.id);
			if (owner === session) saveExportDownload(result);
		} catch (cause) {
			if (owner === session)
				error = cause instanceof Error ? cause.message : 'The export could not be downloaded.';
		} finally {
			if (owner === session) activity = null;
		}
	}
</script>

{#if session}
	<div class="space-y-3 border-t border-hairline p-4" aria-busy={activity !== null}>
		<Button variant="outline" class="min-h-11" disabled={activity !== null} onclick={find}>
			{jobs === null ? 'Find completed exports' : 'Refresh completed exports'}
		</Button>
		{#if activity}<p role="status" class="text-sm text-text-muted"
				>{activity === 'finding' ? 'Finding completed exports…' : 'Verifying download…'}</p
			>{/if}
		{#if jobs?.length === 0 && !activity}
			<p role="status" class="text-sm text-text-muted">No completed exports are available.</p>
		{:else if jobs?.length}
			<ul aria-label="Completed exports" class="divide-y divide-hairline">
				{#each jobs as item (item.id)}
					<li class="flex flex-wrap items-center justify-between gap-2 py-2">
						<div class="min-w-0">
							<p class="text-sm font-medium break-all">{item.fileName ?? item.sourceId}</p>
							<p class="text-xs text-text-muted"
								>{item.sourceId} · {item.streamId} ·
								<time datetime={new Date(item.requestedStartMs).toISOString()}
									>{new Date(item.requestedStartMs).toLocaleString(undefined, {
										timeZone: 'UTC',
										timeZoneName: 'short'
									})}</time
								></p
							>
						</div>
						<Button
							variant="outline"
							class="min-h-11"
							aria-label={`Download ${item.fileName ?? item.sourceId}`}
							disabled={activity !== null}
							onclick={() => void download(item)}>Download</Button
						>
					</li>
				{/each}
			</ul>
		{/if}
		{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	</div>
{/if}
