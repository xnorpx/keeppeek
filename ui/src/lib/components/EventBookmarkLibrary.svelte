<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { resolve } from '$app/paths';
	import { useControlClient } from '$lib/control-context';
	import type { CameraListItem } from '$lib/types';
	import type { EventWorkflow } from '$lib/event-workflow.svelte';
	import {
		eventWorkflowKey,
		type EventWorkflowIdentity,
		type EventWorkflowState
	} from '$lib/event-workflow';
	import EventWorkflowControls from './EventWorkflowControls.svelte';
	import EventWorkflowNotice from './EventWorkflowNotice.svelte';
	import BookmarkIcon from '@lucide/svelte/icons/bookmark';
	import XIcon from '@lucide/svelte/icons/x';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	let {
		date,
		cameras,
		workflow,
		identity,
		onclose
	}: {
		date: string;
		cameras: readonly CameraListItem[];
		workflow: EventWorkflow;
		identity: EventWorkflowIdentity;
		onclose: () => void;
	} = $props();
	const client = useControlClient();
	let dialog = $state<HTMLDialogElement | null>(null);
	let selectedDate = $state(untrack(() => date));
	let sourceId = $state('');
	let byMe = $state(false);
	let records = $state.raw<EventWorkflowState[]>([]);
	let total = $state<number | null>(null);
	let nextPageToken = $state('');
	let loading = $state(true);
	let error = $state<string | null>(null);
	let version = 0;

	onMount(() => {
		dialog?.showModal();
		void load();
		return () => {
			version += 1;
		};
	});

	async function load(token = ''): Promise<void> {
		const current = ++version;
		loading = true;
		error = null;
		try {
			const startMs = Date.parse(`${selectedDate}T00:00:00Z`);
			const page = await client.listEventBookmarks({
				sourceIds: sourceId ? [sourceId] : [],
				startMs,
				endMs: startMs + 86_400_000,
				byMe,
				pageToken: token
			});
			if (current !== version) return;
			workflow.hydrate(page.states);
			records = page.states;
			total = page.total;
			nextPageToken = page.nextPageToken;
		} catch (cause) {
			if (current === version) {
				total = null;
				error = cause instanceof Error ? cause.message : 'Bookmarks could not be loaded.';
			}
		} finally {
			if (current === version) loading = false;
		}
	}

	function eventHref(item: EventWorkflowState): string {
		const parameters = new URLSearchParams({
			date: selectedDate,
			camera: item.sourceId,
			event: item.eventId,
			eventCamera: item.sourceId
		});
		return `${resolve('/events')}?${parameters}`;
	}
</script>

<dialog
	bind:this={dialog}
	class="m-auto max-h-[90dvh] w-[min(48rem,calc(100%-2rem))] overflow-y-auto rounded-md border border-hairline bg-surface p-0 text-foreground backdrop:bg-black/60"
	aria-labelledby="bookmark-library-title"
	{onclose}
>
	<header
		class="sticky top-0 z-10 flex min-h-14 items-center gap-2 border-b border-hairline bg-surface px-4"
	>
		<BookmarkIcon class="size-4 text-primary" />
		<h2 id="bookmark-library-title" class="min-w-0 flex-1 text-sm font-semibold">
			Saved bookmarks
		</h2>
		<button
			type="button"
			class="grid size-11 place-items-center rounded-sm hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring"
			aria-label="Close saved bookmarks"
			title="Close saved bookmarks"
			onclick={() => dialog?.close()}><XIcon class="size-4" /></button
		>
	</header>
	<div class="flex flex-wrap items-center gap-2 border-b border-hairline p-3">
		<label class="min-w-0 text-xs"
			>Bookmark date<input
				type="date"
				bind:value={selectedDate}
				class="ml-2 min-h-11 max-w-full rounded-sm border border-hairline bg-ground px-2"
				onchange={() => void load()}
			/></label
		>
		<label class="min-w-0 flex-1 text-xs"
			><span class="sr-only">Bookmark source</span><select
				aria-label="Bookmark source"
				bind:value={sourceId}
				class="min-h-11 w-full min-w-32 rounded-sm border border-hairline bg-ground px-2"
				onchange={() => void load()}
				><option value="">All authorized sources</option
				>{#each cameras as camera (camera.id)}<option value={camera.id}
						>{camera.name ?? camera.id}</option
					>{/each}</select
			></label
		>
		<label class="inline-flex min-h-11 items-center gap-2 text-xs"
			><input
				type="checkbox"
				bind:checked={byMe}
				class="accent-primary"
				onchange={() => void load()}
			/>Bookmarked by me</label
		>
	</div>
	<div class="flex min-h-11 items-center gap-2 px-4 text-xs text-text-muted" role="status">
		<span data-bookmark-library-count
			>{loading
				? 'Loading bookmarks'
				: total === null
					? 'Count unavailable'
					: `${total} saved bookmarks`}</span
		><button
			type="button"
			class="ml-auto grid size-11 place-items-center rounded-sm hover:bg-raised"
			aria-label="Refresh saved bookmarks"
			title="Refresh saved bookmarks"
			disabled={loading}
			onclick={() => void load()}><RefreshCwIcon class="size-4" /></button
		>
	</div>
	<EventWorkflowNotice {workflow} onchanged={() => void load()} />
	{#if error}<p role="alert" class="p-4 text-sm text-destructive">{error}</p>{/if}
	<div class="divide-y divide-hairline">
		{#each records as item (eventWorkflowKey(item))}
			{@const current = workflow.stateFor(item) ?? item}
			<article data-saved-bookmark={eventWorkflowKey(item)} class="min-w-0 px-4 py-3">
				<div class="flex flex-wrap items-center gap-2 text-sm">
					<h3 class="min-w-0 flex-1 font-medium break-words">
						{current.bookmark?.eventKind ?? 'Event'} · {cameras.find(
							(camera) => camera.id === item.sourceId
						)?.name ?? item.sourceId}
					</h3>
					<time
						class="font-mono text-2xs text-text-muted"
						datetime={new Date(item.bookmark!.eventStartMs).toISOString()}
						>{new Date(item.bookmark!.eventStartMs).toISOString().slice(11, 19)} UTC</time
					>
				</div>
				<EventWorkflowControls
					value={current}
					{workflow}
					{identity}
					detail
					onchanged={() => void load()}
				/>
				{#if current.eventPresent && current.sourceAvailable}<a
						href={eventHref(current)}
						class="inline-flex min-h-11 items-center text-xs font-medium text-primary underline"
						onclick={onclose}>Open event detail</a
					>{/if}
			</article>
		{:else}{#if !loading && !error}<p class="px-4 py-6 text-sm text-text-muted">
					No saved bookmarks for this date.
				</p>{/if}{/each}
	</div>
	{#if nextPageToken}<div class="flex justify-center border-t border-hairline p-3">
			<button
				type="button"
				class="min-h-11 rounded-sm border border-hairline px-4 text-xs disabled:opacity-40"
				disabled={loading}
				onclick={() => void load(nextPageToken)}>Next 16 bookmarks</button
			>
		</div>{/if}
</dialog>
