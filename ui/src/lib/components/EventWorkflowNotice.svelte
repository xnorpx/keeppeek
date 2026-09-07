<script lang="ts">
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import UndoIcon from '@lucide/svelte/icons/undo-2';
	import XIcon from '@lucide/svelte/icons/x';
	import type { EventWorkflow } from '$lib/event-workflow.svelte';
	let { workflow, onchanged }: { workflow: EventWorkflow; onchanged?: () => void } = $props();
	async function finish(action: Promise<boolean>): Promise<void> {
		if (await action) onchanged?.();
	}
</script>

{#if workflow.error}
	<div
		class="flex shrink-0 flex-wrap items-center gap-2 border-y border-destructive/30 bg-destructive/5 px-3 py-2 text-xs"
		role="alert"
	>
		<p class="min-w-0 flex-1 break-words">{workflow.error}</p>
		{#if workflow.retryIntent}
			<button
				type="button"
				class="inline-flex min-h-11 items-center gap-2 rounded-sm border border-hairline px-3 font-medium disabled:opacity-40"
				disabled={workflow.busy}
				onclick={() => void finish(workflow.retry())}
				><RefreshCwIcon class="size-4" />Reload and retry</button
			>
		{/if}
		<button
			type="button"
			class="grid size-11 place-items-center rounded-sm hover:bg-raised"
			aria-label="Dismiss workflow message"
			title="Dismiss workflow message"
			onclick={() => (workflow.error = null)}><XIcon class="size-4" /></button
		>
	</div>
{:else if workflow.notice}
	<div
		class="flex shrink-0 flex-wrap items-center gap-2 border-y border-hairline px-3 py-1 text-xs"
		role="status"
	>
		<p class="min-w-0 flex-1">{workflow.notice}</p>
		{#if workflow.undo}
			<button
				type="button"
				class="inline-flex min-h-11 items-center gap-2 rounded-sm px-3 font-medium hover:bg-raised disabled:opacity-40"
				disabled={workflow.busy}
				aria-label="Undo last review change"
				onclick={() => void finish(workflow.undoLast())}
				><UndoIcon class="size-4" />Undo review: {workflow.undo.scope}</button
			>
		{/if}
	</div>
{/if}
