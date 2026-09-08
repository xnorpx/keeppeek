<script lang="ts">
	import { onMount } from 'svelte';
	import { toJsonString } from '@bufbuild/protobuf';
	import { useControlClient } from '$lib/control-context';
	import {
		RecordingDriftKind,
		RecordingRemedy,
		RecordingReconciliationReportSchema,
		type RecordingReconciliationReport
	} from '$lib/proto/webrtc_pb';
	import SearchIcon from '@lucide/svelte/icons/search';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import XIcon from '@lucide/svelte/icons/x';

	let { enabled }: { enabled: boolean } = $props();
	const client = useControlClient().recordingMaintenance;
	let report = $state.raw<RecordingReconciliationReport | null>(null);
	let busy = $state(false);
	let error = $state('');
	let pendingId = $state('');
	let filter = $state<RecordingDriftKind | ''>('');
	let dialog = $state<HTMLDialogElement>();
	let alive = false;
	let items = $derived(report?.items.filter((item) => filter === '' || item.kind === filter) ?? []);
	let categories = $derived([...new Set(report?.items.map((item) => item.kind) ?? [])]);

	onMount(() => {
		alive = true;
		return () => {
			alive = false;
		};
	});
	$effect(() => {
		if (!dialog) return;
		if (pendingId && !dialog.open) dialog.showModal();
		if (!pendingId && dialog.open) dialog.close();
	});

	async function run(action: () => Promise<RecordingReconciliationReport>): Promise<void> {
		if (busy || !enabled) return;
		busy = true;
		error = '';
		try {
			const result = await action();
			if (alive) {
				report = result;
				pendingId = '';
			}
		} catch (cause) {
			if (alive)
				error = cause instanceof Error ? cause.message : 'Catalog reconciliation is unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}

	function apply(itemId: string, remedy: RecordingRemedy): void {
		const current = report;
		if (current) void run(() => client.applyRemedy(current, itemId, remedy));
	}

	function download(): void {
		if (!report) return;
		const url = URL.createObjectURL(
			new Blob([toJsonString(RecordingReconciliationReportSchema, report)], {
				type: 'application/json'
			})
		);
		const link = document.createElement('a');
		link.href = url;
		link.download = `catalog-reconciliation-${report.reportId}.json`;
		link.click();
		setTimeout(() => URL.revokeObjectURL(url), 0);
	}

	function label(kind: RecordingDriftKind): string {
		return (RecordingDriftKind[kind] ?? 'UNKNOWN').toLowerCase().replaceAll('_', ' ');
	}
</script>

<section class="space-y-4" aria-label="Catalog reconciliation">
	<div class="flex flex-wrap items-center gap-3">
		<h2 class="text-base font-semibold">Catalog reconciliation</h2>
		<div class="flex-1"></div>
		{#if report}<button
				type="button"
				class="grid size-10 place-items-center rounded-sm border border-hairline"
				onclick={download}
				aria-label="Download reconciliation report"
				title="Download reconciliation report"><DownloadIcon class="size-4" /></button
			>{/if}
		<button
			type="button"
			class="flex h-10 items-center justify-center gap-2 rounded-sm border border-hairline-strong px-3 text-xs disabled:opacity-40"
			disabled={busy || !enabled}
			onclick={() => void run(() => client.inspectCatalog())}
			><SearchIcon class="size-4" />{busy ? 'Inspecting…' : 'Inspect catalog'}</button
		>
	</div>
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	{#if report}
		<p role="status" class="text-sm text-text-muted">
			{report.inspected} objects inspected · {report.items.length} findings · revision {report.revision.toString()}
		</p>
		{#if !report.complete}<p role="alert" class="border-l-2 border-destructive pl-3 text-sm">
				Scan limit reached. The report is incomplete; remedies are unavailable.
			</p>{/if}
		<label class="flex items-center gap-3 text-xs text-text-muted"
			>Category<select
				class="h-10 rounded-sm border border-hairline bg-ground px-3 capitalize"
				bind:value={filter}
				><option value="">All findings</option>{#each categories as kind (kind)}<option value={kind}
						>{label(kind)}</option
					>{/each}</select
			></label
		>
		<ul class="divide-y divide-hairline border-y border-hairline">
			{#each items as item (item.itemId)}
				<li class="grid gap-3 py-3 text-xs md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]">
					<div class="min-w-0">
						<p class="capitalize">{label(item.kind)}</p>
						<p class="mt-1 font-mono break-all text-text-muted">{item.recordingId || item.label}</p>
					</div>
					<p class="text-text-muted">
						{item.kind === RecordingDriftKind.UNKNOWN_FILE ||
						item.kind === RecordingDriftKind.TEMPORARY_FILE
							? 'Unindexed bytes remain untouched.'
							: item.kind === RecordingDriftKind.MISSING_FILE
								? 'No file observed at the catalog location.'
								: 'Requires inspection before any change.'}
					</p>
					<div class="flex flex-wrap gap-2">
						{#if item.appliedRemedy !== undefined}<span class="self-center text-text-muted"
								>{item.appliedRemedy === RecordingRemedy.IGNORE
									? 'Acknowledged'
									: 'Catalog tombstone retained'}</span
							>
						{:else}
							{#if item.remedies.includes(RecordingRemedy.IGNORE)}<button
									type="button"
									class="h-10 rounded-sm border border-hairline px-3 disabled:opacity-40"
									disabled={busy || !enabled}
									onclick={() => apply(item.itemId, RecordingRemedy.IGNORE)}>Acknowledge</button
								>{/if}
							{#if item.remedies.includes(RecordingRemedy.RETAIN_TOMBSTONE)}<button
									type="button"
									class="h-10 rounded-sm border border-destructive px-3 text-destructive disabled:opacity-40"
									disabled={busy || !enabled}
									onclick={() => (pendingId = item.itemId)}>Retain tombstone</button
								>{/if}
						{/if}
					</div>
				</li>
			{/each}
		</ul>
		{#if !items.length}<p class="text-sm text-text-muted">No findings in this category.</p>{/if}
	{:else}<p class="text-sm text-text-muted">No catalog report.</p>{/if}
</section>

<dialog
	bind:this={dialog}
	onclose={() => (pendingId = '')}
	class="m-auto max-h-[90dvh] w-[calc(100%-2rem)] max-w-lg rounded-md border border-hairline-strong bg-ground p-4 text-foreground backdrop:bg-black/60"
	aria-labelledby="reconcile-title"
>
	<div class="flex items-center gap-3">
		<h2 id="reconcile-title" class="text-base font-semibold">Remove the missing catalog entry?</h2>
		<button
			type="button"
			class="ml-auto grid size-10 shrink-0 place-items-center"
			onclick={() => (pendingId = '')}
			aria-label="Close remedy confirmation"><XIcon class="size-4" /></button
		>
	</div>
	<p class="my-4 text-sm text-text-muted">
		The file must still be missing. A tombstone is retained, playback indexes are removed, and no
		filesystem bytes are deleted.
	</p>
	{#if error}<p role="alert" class="mb-3 text-sm text-destructive">{error}</p>{/if}
	<div class="flex flex-wrap justify-end gap-3">
		<button
			type="button"
			class="h-11 rounded-sm border border-hairline px-4 text-sm"
			onclick={() => (pendingId = '')}>Cancel</button
		><button
			type="button"
			class="text-destructive-foreground h-11 rounded-sm bg-destructive px-4 text-sm disabled:opacity-40"
			disabled={busy || !enabled}
			onclick={() => apply(pendingId, RecordingRemedy.RETAIN_TOMBSTONE)}>Retain tombstone</button
		>
	</div>
</dialog>
