<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { onMount } from 'svelte';
	import type { ArchiveController } from '$lib/archive';
	import type { SanitizedConfig, SettingsConfigUpdateResponse } from '$lib/types';
	import {
		ArchiveCommandSchema,
		ArchivePrimaryCopyState,
		StorageObjectKind,
		type ArchiveJob,
		type ArchiveStatus
	} from '$lib/proto/webrtc_pb';
	import { Button } from '$lib/components/ui/button/index.js';
	import ArchiveDraftEditor from './ArchiveDraftEditor.svelte';
	import ArchiveCopy from './ArchiveCopy.svelte';
	import ArchiveActionDialog from './ArchiveActionDialog.svelte';
	let {
		config,
		controller,
		onsaved,
		disabled = false
	}: {
		config: SanitizedConfig;
		controller: ArchiveController;
		onsaved: (result: SettingsConfigUpdateResponse) => void;
		disabled?: boolean;
	} = $props();
	let jobs = $state<ArchiveJob[]>([]);
	let status = $state<ArchiveStatus | undefined>();
	let next = $state('');
	let busy = $state(false);
	let editing = $state(false);
	let error = $state('');
	let message = $state('');
	let confirmation = $state<{ kind: 'evict' | 'restore'; job: ArchiveJob } | null>(null);
	let alive = true;
	onMount(() => {
		void refresh();
		return () => {
			alive = false;
		};
	});
	async function refresh(afterJobId = '') {
		busy = true;
		error = '';
		try {
			const response = await controller.archive(
				create(ArchiveCommandSchema, { action: { case: 'list', value: { afterJobId, limit: 16 } } })
			);
			if (!alive) return;
			if (response.result.case !== 'jobs') throw new Error('Unexpected archive job list response.');
			jobs = response.result.value.jobs;
			next = response.result.value.nextAfterJobId;
			status = response.result.value.status;
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Archive status unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function probe() {
		busy = true;
		error = '';
		try {
			const response = await controller.archive(
				create(ArchiveCommandSchema, { action: { case: 'probe', value: {} } })
			);
			if (!alive) return;
			if (response.result.case !== 'status') throw new Error('Unexpected archive probe response.');
			status = response.result.value;
			message = 'Destination probe queued. Refresh archive jobs to see its result.';
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Archive probe unavailable.';
		} finally {
			if (alive) busy = false;
		}
	}
	function received(job: ArchiveJob) {
		jobs = [job, ...jobs.filter((current) => current.jobId !== job.jobId)].slice(0, 16);
		message = 'Archive request accepted. Refresh archive jobs for progress.';
	}
	async function action(kind: 'cancel' | 'retry' | 'restore' | 'evict', job: ArchiveJob) {
		if (busy || disabled) return;
		busy = true;
		error = '';
		try {
			const response = await controller.archive(
				create(ArchiveCommandSchema, { action: { case: kind, value: { jobId: job.jobId } } })
			);
			if (!alive) return;
			if (response.result.case !== 'job') throw new Error('Unexpected archive action response.');
			received(response.result.value);
			confirmation = null;
		} catch (cause) {
			if (alive)
				error =
					cause instanceof Error
						? cause.message
						: 'Archive action was not accepted. Refresh before retrying.';
		} finally {
			if (alive) busy = false;
		}
	}
</script>

<section
	class="mt-4 space-y-4 rounded-sm border border-hairline p-5"
	aria-labelledby="local-archive-heading"
	aria-busy={busy}
>
	<div class="flex flex-wrap items-center justify-between gap-3">
		<h3 id="local-archive-heading" class="text-base font-semibold">Local archive</h3>
		<div class="flex flex-wrap gap-2">
			<Button
				class="min-h-11 sm:min-h-0"
				type="button"
				size="sm"
				variant="outline"
				disabled={busy}
				onclick={() => refresh()}>Refresh archive jobs</Button
			><Button
				class="min-h-11 sm:min-h-0"
				type="button"
				size="sm"
				variant="outline"
				disabled={disabled || editing || busy}
				onclick={() => {
					editing = true;
				}}>Edit archive settings</Button
			>
		</div>
	</div>
	<p class="text-sm text-text-muted">
		Keep verified replicas on one local mounted destination. Archived recordings remain on the
		timeline after primary eviction. Archive access and seeking may be slower.
	</p>
	{#if status}<p role="status" class="text-sm">
			{status.probePending
				? 'Archive destination probe pending.'
				: !status.runtimeAvailable
					? 'Archive runtime unavailable. Save enabled settings and restart to apply them.'
					: status.online
						? 'Archive destination is available.'
						: 'Archive destination unavailable. Recording continues; archived media may be inaccessible.'}
		</p>{/if}
	<Button
		class="min-h-11 sm:min-h-0"
		type="button"
		variant="outline"
		disabled={disabled || busy || !status?.runtimeAvailable || status?.probePending}
		onclick={probe}>Test archive destination</Button
	>
	{#if message}<p role="status" class="text-sm break-words">{message}</p>{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	{#if editing}<ArchiveDraftEditor
			{config}
			{controller}
			oncancel={() => {
				editing = false;
			}}
			onsaved={(result) => {
				editing = false;
				onsaved(result);
				message = 'Archive settings saved. Restart to apply them.';
			}}
		/>{/if}
	<ArchiveCopy
		{controller}
		disabled={disabled || busy || editing || !status?.runtimeAvailable}
		onjob={received}
	/>
	<div class="space-y-3" aria-label="Archive jobs">
		{#if !jobs.length}<p class="text-sm text-text-muted">No archive jobs on this page.</p>{/if}
		{#each jobs as job (job.jobId)}
			<article
				class="min-w-0 space-y-2 rounded-sm border border-hairline p-3 text-sm"
				aria-label={`Archive job ${job.jobId}`}
			>
				<p class="font-medium break-all">
					{job.kind === StorageObjectKind.EXPORT ? 'Export' : 'Recording'} · {job.state}{job.action
						? ` · ${job.action}`
						: ''}
				</p>
				<p class="text-xs break-all text-text-muted">
					Target {job.targetId} · {job.bytes.toString()} bytes · Attempt {job.attempts}{job.action
						? ` · Action attempt ${job.actionAttempts}`
						: ''}
				</p>
				<details class="text-xs text-text-muted">
					<summary class="cursor-pointer focus-visible:outline-2 focus-visible:outline-ring"
						>Job details</summary
					>
					<dl class="mt-2 space-y-1 font-mono break-all">
						<div>
							<dt>Object</dt>
							<dd>{job.objectId}</dd>
						</div>
						<div>
							<dt>Job</dt>
							<dd>{job.jobId}</dd>
						</div>
					</dl>
				</details>
				<p class="text-xs text-text-muted">
					{job.primaryCopy === ArchivePrimaryCopyState.EVICTED
						? 'Primary evicted; archive retained.'
						: job.primaryCopy === ArchivePrimaryCopyState.EVICTING
							? 'Primary eviction is reconciling. Cancellation is unavailable.'
							: 'Primary copy retained.'}
				</p>
				{#if job.errorCode}<p role="alert" class="text-sm text-destructive">
						{job.action === 'restore'
							? 'Restore failed. Check the primary disk and retry. The archive copy is retained.'
							: 'Archive operation failed. Refresh the job and destination status before retrying.'}
					</p>{/if}
				<div class="flex flex-wrap gap-2">
					{#if (job.state === 'queued' || job.state === 'copying' || job.action) && job.primaryCopy !== ArchivePrimaryCopyState.EVICTING}<Button
							class="min-h-11 sm:min-h-0"
							type="button"
							size="sm"
							variant="outline"
							disabled={disabled || busy}
							aria-label={`Cancel archive job ${job.jobId}`}
							onclick={() => action('cancel', job)}>Cancel</Button
						>{/if}
					{#if job.state === 'failed' || job.state === 'cancelled' || (job.errorCode && job.action)}<Button
							class="min-h-11 sm:min-h-0"
							type="button"
							size="sm"
							variant="outline"
							disabled={disabled || busy}
							aria-label={`Retry archive job ${job.jobId}`}
							onclick={() => action('retry', job)}
							>{job.state === 'cancelled' ? 'Retry / finish cleanup' : 'Retry'}</Button
						>{/if}
					{#if job.state === 'ready' && !job.action}
						{#if job.primaryCopy === ArchivePrimaryCopyState.EVICTED}<Button
								class="min-h-11 sm:min-h-0"
								type="button"
								size="sm"
								variant="outline"
								disabled={disabled || busy}
								aria-label={`Restore primary ${job.objectId}`}
								onclick={() => {
									error = '';
									confirmation = { kind: 'restore', job };
								}}>Restore primary</Button
							>{:else if job.primaryCopy === ArchivePrimaryCopyState.PRESENT}<Button
								class="min-h-11 sm:min-h-0"
								type="button"
								size="sm"
								variant="outline"
								disabled={disabled || busy}
								aria-label={`Evict primary ${job.objectId}`}
								onclick={() => {
									error = '';
									confirmation = { kind: 'evict', job };
								}}>Evict primary</Button
							>{/if}
					{/if}
					{#if job.kind === StorageObjectKind.EXPORT}<span class="text-xs text-text-muted"
							>Completed export archive</span
						>{/if}
				</div>
			</article>
		{/each}
	</div>
	{#if next}<Button
			class="min-h-11 sm:min-h-0"
			type="button"
			variant="outline"
			disabled={busy}
			onclick={() => refresh(next)}>Next archive page</Button
		>{/if}
</section>
<ArchiveActionDialog
	request={confirmation}
	busy={busy || disabled}
	{error}
	onclose={() => {
		if (!busy) confirmation = null;
	}}
	onconfirm={() => {
		if (confirmation) void action(confirmation.kind, confirmation.job);
	}}
/>
