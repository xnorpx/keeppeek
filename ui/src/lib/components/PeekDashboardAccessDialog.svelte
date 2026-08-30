<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import type { AccessCredential } from '$lib/access';
	import type { PeekLayout, PeekLayoutAudience } from '$lib/peek-layout';
	import XIcon from '@lucide/svelte/icons/x';
	import PeekDashboardAudiencePicker from './PeekDashboardAudiencePicker.svelte';

	type Props = {
		dashboard: PeekLayout;
		credentials: readonly AccessCredential[];
		busy: boolean;
		onsave: (audience: PeekLayoutAudience) => Promise<boolean>;
		onclose: () => void;
	};

	let { dashboard, credentials, busy, onsave, onclose }: Props = $props();
	let closeButton = $state<HTMLButtonElement | null>(null);
	let audience = $state.raw<PeekLayoutAudience>(
		untrack(() => ({
			everyone: dashboard.audience.everyone,
			credentialIds: [...dashboard.audience.credentialIds]
		}))
	);
	let error = $state<string | null>(null);

	onMount(() => closeButton?.focus());

	async function save(): Promise<void> {
		error = null;
		if (await onsave(audience)) onclose();
		else error = 'Dashboard access was not saved.';
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || busy) return;
		event.preventDefault();
		onclose();
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="fixed inset-0 z-[70] grid place-items-center bg-black/55 p-4" role="presentation">
	<dialog
		open
		class="m-0 w-full max-w-md rounded-md border border-hairline-strong bg-surface p-0 text-foreground shadow-xl"
		aria-modal="true"
		aria-labelledby="peek-dashboard-access-title"
	>
		<header class="flex min-h-14 items-center gap-3 border-b border-hairline px-4">
			<div class="min-w-0 flex-1">
				<h2 id="peek-dashboard-access-title" class="text-sm font-semibold">Dashboard access</h2>
				<p class="truncate text-xs text-text-muted">{dashboard.name}</p>
			</div>
			<button
				bind:this={closeButton}
				type="button"
				class="grid size-8 place-items-center rounded-sm text-text-muted hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				aria-label="Close dashboard access"
				disabled={busy}
				onclick={onclose}
			>
				<XIcon class="size-4" />
			</button>
		</header>
		<div class="p-4">
			<PeekDashboardAudiencePicker
				{credentials}
				{audience}
				onchange={(value) => (audience = value)}
			/>
			{#if error}<p class="mt-3 text-xs text-destructive" role="alert">{error}</p>{/if}
		</div>
		<footer class="flex justify-end gap-2 border-t border-hairline px-4 py-3">
			<button
				type="button"
				class="h-8 rounded-sm border border-hairline bg-raised px-3 text-xs"
				disabled={busy}
				onclick={onclose}>Cancel</button
			>
			<button
				type="button"
				class="h-8 rounded-sm bg-primary px-3 text-xs font-semibold text-primary-foreground disabled:opacity-50"
				disabled={busy}
				onclick={() => void save()}>{busy ? 'Saving…' : 'Save access'}</button
			>
		</footer>
	</dialog>
</div>
