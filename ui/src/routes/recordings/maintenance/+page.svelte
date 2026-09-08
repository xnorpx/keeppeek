<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount, untrack } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import { useCapabilityState } from '$lib/capability-context';
	import RecordingReconciliation from '$lib/components/RecordingReconciliation.svelte';
	import { RECORDING_MAINTENANCE_CAPABILITY } from '$lib/control-client-maintenance';
	import {
		RecordingDeletionJobSchema,
		RecordingDeletionReason,
		RecordingDeletionStatus,
		type RecordingDeletionJob
	} from '$lib/proto/webrtc_pb';
	import { toJsonString } from '@bufbuild/protobuf';
	import type { CameraListItem } from '$lib/types';
	import ArrowLeftIcon from '@lucide/svelte/icons/arrow-left';
	import TrashIcon from '@lucide/svelte/icons/trash-2';
	import RefreshIcon from '@lucide/svelte/icons/refresh-cw';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import XIcon from '@lucide/svelte/icons/x';

	const control = useControlClient();
	const client = control.recordingMaintenance;
	const capabilities = useCapabilityState();
	const previewCommand = 'recording-maintenance-preview';
	let administrator = $state(false);
	let principalId = $state<string | undefined>();
	let view = $state<'deletion' | 'reconciliation'>('deletion');
	let cameras = $state.raw<CameraListItem[]>([]);
	let sourceId = $state(page.url.searchParams.get('camera') ?? '');
	let streamId = $state(page.url.searchParams.get('stream') ?? 'sub');
	let start = $state('');
	let end = $state('');
	let reason = $state(RecordingDeletionReason.OPERATOR);
	let job = $state.raw<RecordingDeletionJob | null>(null);
	let history = $state.raw<RecordingDeletionJob[]>([]);
	let nextJobId = $state('');
	let busy = $state(false);
	let message = $state('');
	let confirmation = $state('');
	let confirmationReady = $state(false);
	let previewOpen = $state(false);
	let dialog = $state<HTMLDialogElement>();
	let alive = false;
	let generation = 0;
	let pollFailures = $state(0);
	let supported = $derived(capabilities.supports(RECORDING_MAINTENANCE_CAPABILITY));
	let capabilityLost = $derived(capabilities.command(previewCommand)?.capabilityLost ?? false);
	let running = $derived(
		job !== null &&
			[
				RecordingDeletionStatus.QUEUED,
				RecordingDeletionStatus.WORKING,
				RecordingDeletionStatus.STAGED
			].includes(job.status)
	);

	onMount(() => {
		alive = true;
		const now = Date.now();
		start = new Date(Number(page.url.searchParams.get('start')) || now - 3_600_000)
			.toISOString()
			.slice(0, 16);
		end = new Date(Number(page.url.searchParams.get('end')) || now).toISOString().slice(0, 16);
		const unsubscribe = control.onAccessState((access) => {
			if (access.session?.role !== 'administrator' || principalId !== access.session?.principalId) {
				invalidateReview();
			}
			principalId = access.session?.principalId;
			administrator = access.session?.role === 'administrator';
		});
		void control.getCameras().then(
			(result) => {
				if (!alive) return;
				cameras = result;
				if (!sourceId) sourceId = result[0]?.id ?? '';
			},
			(cause: unknown) => {
				if (alive) message = failure(cause);
			}
		);
		return () => {
			alive = false;
			generation += 1;
			client.discardConfirmations();
			unsubscribe();
		};
	});

	$effect(() => {
		if (!administrator || !supported || !principalId) return;
		let current = true;
		void client.list().then(
			(result) => {
				if (current) {
					history = result.jobs;
					nextJobId = result.nextJobId;
				}
			},
			(cause: unknown) => {
				if (current) message = failure(cause);
			}
		);
		return () => {
			current = false;
		};
	});

	$effect(() => {
		if (!supported || capabilityLost) untrack(invalidateReview);
	});

	$effect(() => {
		if (!dialog) return;
		if (previewOpen && !dialog.open) dialog.showModal();
		else if (!previewOpen && dialog.open) dialog.close();
	});

	$effect(() => {
		const current = job;
		if (!administrator || !supported || !current || !running || pollFailures >= 3) return;
		let active = true;
		const timer = setTimeout(() => {
			void client.get(current.jobId).then(
				(result) => {
					if (active && job?.jobId === current.jobId) {
						job = result;
						pollFailures = 0;
					}
				},
				(cause: unknown) => {
					if (active) {
						message = failure(cause);
						pollFailures += 1;
					}
				}
			);
		}, 1_000);
		return () => {
			active = false;
			clearTimeout(timer);
		};
	});

	function dismissPreview(): void {
		generation += 1;
		client.discardConfirmations();
		confirmation = '';
		confirmationReady = false;
		previewOpen = false;
		busy = false;
	}

	function invalidateReview(): void {
		dismissPreview();
		job = null;
		history = [];
		nextJobId = '';
	}

	async function perform(action: () => Promise<RecordingDeletionJob>): Promise<boolean> {
		if (busy || !administrator || !supported) return false;
		const request = ++generation;
		busy = true;
		message = '';
		try {
			const result = await action();
			if (!alive || request !== generation || !administrator || !supported) {
				client.discardPreview(result);
				return false;
			}
			job = result;
			pollFailures = 0;
			return true;
		} catch (cause) {
			if (alive && request === generation) message = failure(cause);
			return false;
		} finally {
			if (alive && request === generation) busy = false;
		}
	}

	async function preview(): Promise<void> {
		if (busy || !administrator || !supported) return;
		if (!capabilities.begin(previewCommand, RECORDING_MAINTENANCE_CAPABILITY)) return;
		confirmationReady = false;
		confirmation = '';
		const accepted = await perform(() =>
			client.preview(
				{ sourceId, streamId, startMs: Date.parse(`${start}Z`), endMs: Date.parse(`${end}Z`) },
				reason
			)
		);
		if (!accepted || !alive || !administrator || !supported || capabilityLost) return;
		confirmation = '';
		confirmationReady = job?.status === RecordingDeletionStatus.PREPARED && !message;
		previewOpen = confirmationReady;
	}

	async function confirm(): Promise<void> {
		const selected = job;
		if (!selected || busy || !confirmationReady || !administrator || !supported || capabilityLost)
			return;
		const request = generation + 1;
		confirmationReady = false;
		await perform(() => client.confirm(selected, confirmation));
		if (!alive || request !== generation) return;
		confirmation = '';
		if (!message) previewOpen = false;
	}

	async function nextHistory(): Promise<void> {
		if (!nextJobId || busy || !administrator || !supported) return;
		const request = ++generation;
		busy = true;
		try {
			const result = await client.list(nextJobId);
			if (alive && request === generation) {
				history = result.jobs;
				nextJobId = result.nextJobId;
			}
		} catch (cause) {
			if (alive && request === generation) message = failure(cause);
		} finally {
			if (alive && request === generation) busy = false;
		}
	}

	function download(): void {
		if (!job) return;
		const url = URL.createObjectURL(
			new Blob([toJsonString(RecordingDeletionJobSchema, job)], { type: 'application/json' })
		);
		const link = document.createElement('a');
		link.href = url;
		link.download = `recording-maintenance-${job.jobId}.json`;
		link.click();
		setTimeout(() => URL.revokeObjectURL(url), 0);
	}

	function failure(cause: unknown): string {
		return cause instanceof Error ? cause.message : 'Recording maintenance is unavailable.';
	}
	function timestamp(value: bigint | undefined): string {
		return value === undefined
			? 'Unknown end'
			: new Date(Number(value)).toISOString().replace('T', ' ').replace('.000Z', ' UTC');
	}
	function bytes(value: bigint): string {
		return `${(Number(value) / 1_048_576).toFixed(2)} MiB`;
	}
	function status(value: RecordingDeletionStatus): string {
		return (RecordingDeletionStatus[value] ?? 'UNKNOWN').toLowerCase().replaceAll('_', ' ');
	}
</script>

<svelte:head><title>Recording maintenance - KeepPeek</title></svelte:head>

<div class="h-full overflow-y-auto bg-ground">
	<header class="flex min-h-14 flex-wrap items-center gap-3 border-b border-hairline px-4 py-3">
		<a
			href={resolve('/recordings')}
			class="grid size-10 place-items-center rounded-sm border border-hairline"
			aria-label="Back to recording integrity"
			title="Back to recording integrity"><ArrowLeftIcon class="size-4" /></a
		>
		<h1 class="text-lg font-semibold">Recording maintenance</h1>
	</header>
	<div class="mx-auto max-w-6xl space-y-6 p-4 md:p-6">
		{#if !administrator}<p role="status" class="text-sm text-text-muted">
				Administrator access is required.
			</p>
		{:else}
			{#if !supported}<p role="status" class="border-l-2 border-destructive pl-3 text-sm">
					Recording maintenance is unavailable on this server.
				</p>{/if}
			<nav aria-label="Maintenance views" class="flex gap-1 border-b border-hairline text-sm">
				<button
					type="button"
					class="border-b-2 px-4 py-3 {view === 'deletion'
						? 'border-foreground'
						: 'border-transparent text-text-muted'}"
					aria-pressed={view === 'deletion'}
					onclick={() => (view = 'deletion')}>Recordings</button
				><button
					type="button"
					class="border-b-2 px-4 py-3 {view === 'reconciliation'
						? 'border-foreground'
						: 'border-transparent text-text-muted'}"
					aria-pressed={view === 'reconciliation'}
					onclick={() => (view = 'reconciliation')}>Reconciliation</button
				>
			</nav>
			{#if view === 'reconciliation'}<RecordingReconciliation
					enabled={supported && administrator}
				/>{:else}
				<form
					class="grid gap-3 border-b border-hairline pb-5 lg:grid-cols-3 sm:grid-cols-2"
					onsubmit={(event) => {
						event.preventDefault();
						void preview();
					}}
				>
					<label class="grid gap-1 text-xs text-text-muted"
						>Camera<select
							class="h-10 min-w-0 rounded-sm border border-hairline-strong bg-ground px-2 text-sm text-foreground"
							bind:value={sourceId}
							disabled={busy || !supported}
							>{#each cameras as camera (camera.id)}<option value={camera.id}
									>{camera.name || camera.id}</option
								>{/each}</select
						></label
					>
					<label class="grid gap-1 text-xs text-text-muted"
						>Stream<select
							class="h-10 rounded-sm border border-hairline-strong bg-ground px-2 text-sm text-foreground"
							bind:value={streamId}
							disabled={busy || !supported}
							><option value="sub">Sub</option><option value="main">Main</option></select
						></label
					>
					<label class="grid gap-1 text-xs text-text-muted"
						>Reason<select
							class="h-10 rounded-sm border border-hairline-strong bg-ground px-2 text-sm text-foreground"
							bind:value={reason}
							disabled={busy || !supported}
							><option value={RecordingDeletionReason.OPERATOR}>Operator cleanup</option><option
								value={RecordingDeletionReason.PRIVACY}>Privacy request</option
							></select
						></label
					>
					<label class="grid gap-1 text-xs text-text-muted"
						>Start (UTC)<input
							class="h-10 min-w-0 rounded-sm border border-hairline-strong bg-ground px-2 text-sm text-foreground"
							type="datetime-local"
							required
							bind:value={start}
							disabled={busy || !supported}
						/></label
					>
					<label class="grid gap-1 text-xs text-text-muted"
						>End (UTC)<input
							class="h-10 min-w-0 rounded-sm border border-hairline-strong bg-ground px-2 text-sm text-foreground"
							type="datetime-local"
							required
							bind:value={end}
							disabled={busy || !supported}
						/></label
					>
					<button
						type="submit"
						class="flex h-10 items-center justify-center gap-2 self-end rounded-sm border border-destructive px-4 text-sm text-destructive disabled:opacity-40"
						disabled={busy || !supported || !sourceId}
						><TrashIcon class="size-4" />Preview deletion</button
					>
				</form>
				{#if message}<p role="alert" class="text-sm break-words text-destructive">{message}</p>{/if}
				{#if job}
					<section aria-label="Deletion progress" class="space-y-3">
						<div class="flex flex-wrap items-center gap-3">
							<h2 class="text-base font-semibold capitalize">{status(job.status)}</h2>
							<span class="text-xs text-text-muted"
								>{job.deletedCount} deleted · {job.failedCount} failed · {bytes(job.bytes)}</span
							>
							<div class="flex-1"></div>
							{#if job.jobId}<button
									class="grid size-10 place-items-center rounded-sm border border-hairline"
									type="button"
									disabled={busy}
									onclick={() => void perform(() => client.get(job!.jobId))}
									aria-label="Refresh job"
									title="Refresh job"><RefreshIcon class="size-4" /></button
								>{/if}
							<button
								class="grid size-10 place-items-center rounded-sm border border-hairline"
								type="button"
								onclick={download}
								aria-label="Download report"
								title="Download report"><DownloadIcon class="size-4" /></button
							>
							{#if running || job.status === RecordingDeletionStatus.PREPARED}<button
									class="h-10 rounded-sm border border-hairline px-3 text-xs"
									type="button"
									disabled={busy}
									onclick={() => void perform(() => client.cancel(job!.jobId))}>Cancel job</button
								>{/if}
							{#if job.jobId && (job.failedCount > 0 || job.status === RecordingDeletionStatus.FAILED)}<button
									class="h-10 rounded-sm border border-destructive px-3 text-xs text-destructive"
									type="button"
									disabled={busy}
									onclick={() => void perform(() => client.retry(job!.jobId))}
									>Retry failed objects</button
								>{/if}
						</div>
						{@render objects(job)}
					</section>
				{/if}
				<section aria-label="Maintenance history" class="space-y-3 border-t border-hairline pt-4">
					<h2 class="text-base font-semibold">History</h2>
					{#if history.length === 0}<p class="text-sm text-text-muted">No maintenance jobs.</p>{/if}
					<ul class="divide-y divide-hairline">
						{#each history as entry (entry.jobId)}<li>
								<button
									class="hover:bg-panel flex w-full flex-wrap items-center gap-x-4 gap-y-1 py-3 text-left text-xs"
									type="button"
									disabled={busy}
									onclick={() => void perform(() => client.get(entry.jobId))}
									><span class="font-mono">{entry.jobId.slice(0, 12)}</span><span class="capitalize"
										>{status(entry.status)}</span
									><span>{timestamp(entry.createdAtMs)}</span><span>{bytes(entry.bytes)}</span
									></button
								>
							</li>{/each}
					</ul>
					{#if nextJobId}<button
							type="button"
							class="h-10 rounded-sm border border-hairline px-3 text-xs"
							disabled={busy}
							onclick={() => void nextHistory()}>Next jobs</button
						>{/if}
				</section>
			{/if}
		{/if}
	</div>
</div>

{#snippet objects(value: RecordingDeletionJob)}
	<ul aria-label="Recording objects" class="divide-y divide-hairline border-y border-hairline">
		{#each value.objects as object (object.recordingId)}<li
				class="grid min-w-0 gap-2 py-3 text-xs sm:grid-cols-[minmax(0,1fr)_2fr_auto_auto]"
			>
				<span class="font-mono break-all text-text-muted">{object.recordingId}</span><span
					>{timestamp(object.startMs)}<br />{timestamp(object.endMs)}</span
				><span>{bytes(object.bytes)}</span><span
					class="capitalize {object.error ? 'text-destructive' : ''}"
					>{object.error || status(object.status)}</span
				>
			</li>{/each}
	</ul>
{/snippet}

<dialog
	bind:this={dialog}
	oncancel={dismissPreview}
	onclose={() => {
		if (previewOpen && !dialog?.open) dismissPreview();
	}}
	class="m-auto max-h-[90dvh] w-[calc(100%-2rem)] max-w-3xl overflow-y-auto rounded-md border border-hairline-strong bg-ground p-0 text-foreground backdrop:bg-black/60"
	aria-labelledby="delete-title"
>
	{#if job}
		<div class="flex items-center gap-3 border-b border-hairline p-4">
			<TrashIcon class="size-5 shrink-0 text-destructive" />
			<h2 id="delete-title" class="text-base font-semibold">
				Delete {job.objects.length}
				{job.objects.length === 1 ? 'recording' : 'recordings'} permanently?
			</h2>
			<button
				class="ml-auto grid size-10 shrink-0 place-items-center"
				type="button"
				onclick={dismissPreview}
				aria-label="Close preview"><XIcon class="size-4" /></button
			>
		</div>
		<div class="space-y-4 p-4">
			<p class="text-sm">
				{bytes(job.bytes)} · whole-recording boundaries · catalog revision {job.revision.toString()}
			</p>
			{@render objects(job)}
			<dl class="grid grid-cols-3 gap-3 text-xs">
				<div>
					<dt class="text-text-muted">Gaps</dt>
					<dd>{job.gaps.length}</dd>
				</div>
				<div>
					<dt class="text-text-muted">Bookmarks</dt>
					<dd>{job.bookmarkEventIds.length}</dd>
				</div>
				<div>
					<dt class="text-text-muted">Related exports</dt>
					<dd>{job.relatedExportIds.length}</dd>
				</div>
			</dl>
			{#if job.gaps.length}<details class="text-xs">
					<summary class="cursor-pointer py-2">Coverage gaps</summary>
					<ul>
						{#each job.gaps as gap, index (index)}<li class="py-1">
								{timestamp(gap.startMs)} to {timestamp(gap.endMs)}
							</li>{/each}
					</ul>
				</details>{/if}
			<ul class="space-y-1 text-xs text-text-muted">
				{#each job.consequences as consequence (consequence)}<li>{consequence}</li>{/each}
			</ul>
			<p class="text-xs text-text-muted">
				Confirmation expires {timestamp(job.expiresAtMs)}. This cannot be undone.
			</p>
			{#if message}<p role="alert" class="text-sm text-destructive">{message}</p>{/if}
			<label class="grid gap-2 text-sm"
				>Type {job.requiredConfirmationText}<input
					class="h-11 rounded-sm border border-hairline-strong bg-ground px-3 font-mono"
					bind:value={confirmation}
					disabled={busy || !confirmationReady || !administrator || !supported || capabilityLost}
					autocomplete="off"
					spellcheck="false"
				/></label
			>
			<div class="flex flex-wrap justify-end gap-3">
				{#if !confirmationReady}<button
						class="flex h-11 items-center gap-2 rounded-sm border border-hairline px-4 text-sm"
						type="button"
						disabled={busy || !supported || !administrator}
						onclick={() => void preview()}><RefreshIcon class="size-4" />Refresh preview</button
					>{/if}
				<button
					class="h-11 rounded-sm border border-hairline px-4 text-sm"
					type="button"
					onclick={dismissPreview}>Keep recordings</button
				><button
					class="text-destructive-foreground flex h-11 items-center justify-center gap-2 rounded-sm bg-destructive px-4 text-sm disabled:opacity-40"
					type="button"
					disabled={busy ||
						!confirmationReady ||
						capabilityLost ||
						!supported ||
						!administrator ||
						confirmation !== job.requiredConfirmationText}
					onclick={() => void confirm()}><TrashIcon class="size-4" />Delete permanently</button
				>
			</div>
		</div>
	{/if}
</dialog>
