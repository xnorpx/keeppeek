<script lang="ts">
	import BookmarkIcon from '@lucide/svelte/icons/bookmark';
	import CheckCheckIcon from '@lucide/svelte/icons/check-check';
	import EyeOffIcon from '@lucide/svelte/icons/eye-off';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import SaveIcon from '@lucide/svelte/icons/save';
	import XIcon from '@lucide/svelte/icons/x';
	import {
		EVENT_WORKFLOW_NOTE_BYTES,
		eventWorkflowKey,
		type EventWorkflowIdentity,
		type EventWorkflowState
	} from '$lib/event-workflow';
	import type { EventWorkflow } from '$lib/event-workflow.svelte';

	type Props = {
		value: EventWorkflowState;
		workflow: EventWorkflow;
		identity: EventWorkflowIdentity;
		detail?: boolean;
		onchanged?: () => void;
	};
	let { value, workflow, identity, detail = false, onchanged }: Props = $props();
	let editing = $state(false);
	let note = $state('');
	let noteRevision = $state('0');
	let current = $derived(workflow.stateFor(value) ?? value);
	let canEdit = $derived(
		!current.bookmark || current.bookmark.createdBy === identity.actorId || identity.administrator
	);
	let noteBytes = $derived(new TextEncoder().encode(note).byteLength);
	const inputId = $props.id();

	async function finish(action: Promise<boolean>): Promise<void> {
		if (await action) onchanged?.();
	}

	function editNote(): void {
		note = current.bookmark?.note ?? '';
		noteRevision = current.bookmark?.revision ?? '0';
		editing = true;
	}

	async function saveNote(): Promise<void> {
		if (noteBytes > EVENT_WORKFLOW_NOTE_BYTES) return;
		if (await workflow.bookmark(current, true, note, noteRevision)) {
			editing = false;
			onchanged?.();
		}
	}
</script>

<div data-workflow-event={eventWorkflowKey(value)} class="min-w-0">
	<div class="flex flex-wrap items-center gap-1" role="group" aria-label="Event review actions">
		<button
			type="button"
			class="grid size-11 shrink-0 place-items-center rounded-sm text-text-muted hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40 aria-pressed:text-primary"
			aria-label={current.reviewed ? 'Mark event unreviewed' : 'Mark event reviewed'}
			title={current.reviewed ? 'Mark event unreviewed' : 'Mark event reviewed'}
			aria-pressed={current.reviewed}
			disabled={workflow.busy || !current.eventPresent}
			onclick={() => void finish(workflow.review([current], { reviewed: !current.reviewed }, '1'))}
		>
			<CheckCheckIcon class="size-4" />
		</button>
		<button
			type="button"
			class="grid size-11 shrink-0 place-items-center rounded-sm text-text-muted hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40 aria-pressed:text-primary"
			aria-label={current.dismissed ? 'Restore dismissed event' : 'Dismiss event'}
			title={current.dismissed ? 'Restore dismissed event' : 'Dismiss event'}
			aria-pressed={current.dismissed}
			disabled={workflow.busy || !current.eventPresent}
			onclick={() =>
				void finish(workflow.review([current], { dismissed: !current.dismissed }, '1'))}
		>
			<EyeOffIcon class="size-4" />
		</button>
		<button
			type="button"
			class="grid size-11 shrink-0 place-items-center rounded-sm text-text-muted hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40 aria-pressed:text-primary"
			aria-label={current.bookmark?.active ? 'Remove bookmark' : 'Bookmark event'}
			title={!canEdit
				? 'Only the bookmark creator or an administrator can change this bookmark'
				: current.bookmark?.active
					? 'Remove bookmark'
					: 'Bookmark event'}
			aria-pressed={current.bookmark?.active ?? false}
			disabled={workflow.busy || !canEdit || (!current.eventPresent && !current.bookmark)}
			onclick={() =>
				void finish(
					workflow.bookmark(current, !current.bookmark?.active, current.bookmark?.note ?? '')
				)}
		>
			<BookmarkIcon class="size-4" fill={current.bookmark?.active ? 'currentColor' : 'none'} />
		</button>
		{#if detail && current.bookmark?.active}
			<span class="min-w-0 flex-1 text-xs text-text-muted">Shared bookmark</span>
			{#if canEdit}
				<button
					type="button"
					class="grid size-11 shrink-0 place-items-center rounded-sm text-text-muted hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					aria-label="Edit bookmark note"
					title="Edit bookmark note"
					disabled={workflow.busy}
					onclick={editNote}><PencilIcon class="size-4" /></button
				>
			{/if}
		{/if}
	</div>
	{#if detail && current.bookmark?.active && (!current.eventPresent || !current.mediaAvailable || !current.sourceAvailable)}
		<p class="text-warning py-1 text-xs">
			{!current.sourceAvailable
				? 'Source unavailable; metadata only'
				: !current.eventPresent
					? 'Event removed; metadata only'
					: 'Recording unavailable; metadata only'}
		</p>
	{/if}
	{#if detail && editing}
		<form
			class="space-y-2 py-2"
			onsubmit={(event) => {
				event.preventDefault();
				void saveNote();
			}}
		>
			<label for={inputId} class="block text-xs font-medium">Bookmark note</label>
			<textarea
				id={inputId}
				class="min-h-24 w-full resize-y rounded-sm border border-hairline bg-ground p-2 text-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				bind:value={note}
				maxlength={EVENT_WORKFLOW_NOTE_BYTES}
				aria-invalid={noteBytes > EVENT_WORKFLOW_NOTE_BYTES}
				aria-describedby={`${inputId}-limit`}
				onkeydown={(event) => {
					if (event.key === 'Escape') {
						event.stopPropagation();
						editing = false;
					}
				}}></textarea>
			<p id={`${inputId}-limit`} class="font-mono text-2xs text-text-muted">
				{noteBytes} / {EVENT_WORKFLOW_NOTE_BYTES} bytes
			</p>
			<div class="flex items-center gap-2">
				<button
					type="submit"
					class="inline-flex min-h-11 items-center gap-2 rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary disabled:opacity-40"
					disabled={workflow.busy || noteBytes > EVENT_WORKFLOW_NOTE_BYTES}
					><SaveIcon class="size-4" />Save bookmark note</button
				>
				<button
					type="button"
					class="grid size-11 place-items-center rounded-sm hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring"
					aria-label="Cancel note editing"
					title="Cancel note editing"
					onclick={() => (editing = false)}><XIcon class="size-4" /></button
				>
			</div>
		</form>
	{:else if detail && current.bookmark?.active && current.bookmark.note}
		<p class="py-2 text-sm break-words whitespace-pre-wrap">{current.bookmark.note}</p>
	{/if}
	{#if detail && current.bookmark?.audit.length}
		<details class="py-2 text-xs text-text-muted">
			<summary class="cursor-pointer">Bookmark history</summary>
			<ul class="mt-2 space-y-2">
				{#each current.bookmark.audit as entry (entry.revision)}
					<li class="break-words">
						<time datetime={new Date(entry.occurredAtMs).toISOString()}
							>{new Date(entry.occurredAtMs).toISOString().replace('T', ' ').slice(0, 19)} UTC</time
						>: {entry.action} by {entry.actorId === identity.actorId ? 'you' : entry.actorId}
					</li>
				{/each}
			</ul>
		</details>
	{/if}
</div>
