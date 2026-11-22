<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { onMount, tick } from 'svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import type { ArchiveController } from '$lib/archive';
	import {
		ArchiveCommandSchema,
		StorageObjectKind,
		type ArchiveDeletionPreview
	} from '$lib/proto/webrtc_pb';

	type Selection = { jobId: string } | { after: string };
	let {
		controller,
		selection,
		disabled,
		onclose
	}: {
		controller: ArchiveController;
		selection: Selection;
		disabled: boolean;
		onclose: () => void;
	} = $props();
	const id = $props.id();
	let dialog: HTMLDialogElement;
	let closeButton = $state<HTMLButtonElement | null>(null);
	let plan = $state.raw<ArchiveDeletionPreview | null>(null);
	let loading = $state(false);
	let error = $state('');
	let alive = true;
	let selected: Selection;
	const states: Record<string, string> = {
		eligible: 'Eligible for deletion',
		retained: 'Within retention period',
		restore_required: 'Restore the primary copy first',
		primary_unavailable: 'Verified primary unavailable',
		preservation_hold: 'Preservation hold or maintenance',
		active_reader: 'An active reader is using this object',
		busy: 'Another operation is in progress',
		verification_date_unknown: 'Awaiting fresh verification',
		queued: 'Deletion queued',
		claimed: 'Deleting',
		retired: 'Archive copy removed',
		acknowledged: 'Archive copy removed',
		blocked: 'Deletion blocked'
	};
	onMount(() => {
		dialog.showModal();
		closeButton?.focus();
		void preview(selection);
		return () => {
			alive = false;
		};
	});
	async function finishRequest() {
		if (!alive) return;
		loading = false;
		await tick();
		if (alive) closeButton?.focus();
	}
	async function preview(scope: Selection) {
		if (loading || disabled) return;
		selected = scope;
		loading = true;
		error = '';
		plan = null;
		try {
			const selection =
				'jobId' in scope
					? { case: 'jobId' as const, value: scope.jobId }
					: { case: 'retentionPage' as const, value: { afterJobId: scope.after, limit: 16 } };
			const result = await controller.archive(
				create(ArchiveCommandSchema, { action: { case: 'previewDeletion', value: { selection } } })
			);
			if (result.result.case !== 'deletionPreview')
				throw new Error('Archive deletion preview unavailable.');
			if (alive) plan = result.result.value;
		} catch (cause) {
			if (alive)
				error = cause instanceof Error ? cause.message : 'Could not preview archive deletion.';
		} finally {
			await finishRequest();
		}
	}
	async function confirm() {
		if (disabled || loading || !plan || plan.confirmed || !plan.selectedCount) return;
		loading = true;
		error = '';
		try {
			const result = await controller.archive(
				create(ArchiveCommandSchema, {
					action: {
						case: 'confirmDeletion',
						value: {
							planId: plan.planId,
							expectedRevision: plan.revision,
							confirmationNonce: plan.confirmationNonce
						}
					}
				})
			);
			if (result.result.case !== 'deletionPreview' || !result.result.value.confirmed)
				throw new Error('Archive deletion confirmation unavailable.');
			if (alive) plan = result.result.value;
		} catch (cause) {
			if (alive)
				error = cause instanceof Error ? cause.message : 'Could not confirm archive deletion.';
		} finally {
			await finishRequest();
		}
	}
</script>

<dialog
	bind:this={dialog}
	{onclose}
	oncancel={(event) => {
		if (loading) event.preventDefault();
	}}
	onkeydown={(event) => {
		if (event.key === 'Escape') event.stopPropagation();
	}}
	aria-labelledby={`${id}-title`}
	class="m-auto max-h-[85dvh] w-[calc(100%-2rem)] max-w-lg space-y-4 overflow-auto rounded-md border border-hairline-strong bg-surface p-5 text-foreground shadow-xl backdrop:bg-black/55"
>
	<h2 id={`${id}-title`} class="text-lg-plus font-semibold">Archive deletion preview</h2>
	<p role="status" class="text-sm break-words">
		{#if loading}Checking archive deletion…{:else if plan?.confirmed}Batch confirmed. Refresh
			archive jobs to follow progress.{:else if plan}Target {plan.targetId} · {plan.selectedCount} selected
			· {plan.selectedBytes.toString()} bytes{/if}
	</p>
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	{#if plan}
		{#if !plan.confirmed}
			<p class="text-sm"
				>Confirming queues removal of the selected archive copies. Each primary copy is verified
				again before removal. If the filesystem cannot protect the primary from writes, KeepPeek
				retains both copies.</p
			>
		{/if}
		{#if !plan.confirmed && plan.selectedCount > 0}
			<p class="text-sm"
				>After removal, KeepPeek permanently excludes these objects from automatic archiving and the
				manual copy picker.</p
			>
		{/if}
		{#if !plan.items.length}<p class="text-sm">No archive copies on this page.</p>{/if}
		<ul class="space-y-2 text-sm">
			{#each plan.items as item (item.jobId)}
				<li class="min-w-0 rounded-sm border border-hairline p-2">
					<p class="break-all"
						>{item.kind === StorageObjectKind.EXPORT ? 'Export' : 'Recording'} · {item.objectId}</p
					>
					<p>{states[item.state] ?? 'Unavailable'} · {item.bytes.toString()} bytes</p>
					{#if item.verifiedAtMs !== undefined}<p
							>Verified {new Date(Number(item.verifiedAtMs)).toLocaleString()}</p
						>{/if}
				</li>
			{/each}
		</ul>
	{/if}
	<div class="flex flex-wrap gap-2">
		<Button
			type="button"
			variant="outline"
			class="min-h-11 sm:min-h-0"
			bind:ref={closeButton}
			disabled={loading}
			onclick={() => dialog.close()}>Close</Button
		>
		{#if error}<Button
				type="button"
				variant="outline"
				class="min-h-11 sm:min-h-0"
				disabled={loading || disabled}
				onclick={() => preview(selected)}>Preview again</Button
			>{/if}
		{#if plan && !plan.confirmed && plan.selectedCount > 0}
			<Button
				type="button"
				variant="destructive"
				class="min-h-11 sm:min-h-0"
				disabled={loading || disabled}
				onclick={confirm}>Confirm archive deletion</Button
			>
		{/if}
		{#if plan?.nextAfterJobId}
			<Button
				type="button"
				variant="outline"
				class="min-h-11 sm:min-h-0"
				disabled={loading || disabled}
				onclick={() => {
					if (plan) void preview({ after: plan.nextAfterJobId });
				}}>Preview next page</Button
			>
		{/if}
	</div>
</dialog>
