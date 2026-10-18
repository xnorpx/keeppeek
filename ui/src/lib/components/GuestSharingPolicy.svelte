<script lang="ts">
	import { onMount } from 'svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import { canonicalShareOrigin, type SharingControlClient } from '$lib/control-client-sharing';
	import type { SharePolicy } from '$lib/proto/webrtc_pb';

	let { controller }: { controller: Pick<SharingControlClient, 'getPolicy' | 'configurePolicy'> } =
		$props();
	const id = $props.id();
	let policy = $state.raw<SharePolicy | null>(null);
	let enabled = $state(false);
	let origin = $state('');
	let loading = $state(true);
	let saving = $state(false);
	let mustReload = $state(false);
	let error = $state<string | null>(null);
	let status = $state<string | null>(null);
	let generation = 0;

	onMount(() => {
		void load();
		return () => {
			generation += 1;
		};
	});

	function apply(value: SharePolicy): void {
		policy = value;
		enabled = value.settings?.enabled ?? false;
		origin = value.settings?.publicOrigin ?? '';
		mustReload = false;
	}

	async function load(): Promise<void> {
		const version = ++generation;
		loading = true;
		error = null;
		status = null;
		try {
			const value = await controller.getPolicy();
			if (version === generation) apply(value);
		} catch {
			if (version === generation)
				error = 'The sharing policy could not be loaded. Reload to try again.';
		} finally {
			if (version === generation) loading = false;
		}
	}

	async function save(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (!policy || saving || mustReload) return;
		error = null;
		status = null;
		const publicOrigin = origin.trim() || undefined;
		try {
			if (publicOrigin !== undefined) canonicalShareOrigin(publicOrigin);
			if (enabled && !publicOrigin)
				throw new Error('Enter a reachable HTTPS origin to enable sharing.');
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Review the external HTTPS origin.';
			return;
		}
		const version = generation;
		saving = true;
		try {
			const value = await controller.configurePolicy({ enabled, publicOrigin }, policy.revision);
			if (version !== generation) return;
			apply(value);
			status = 'Sharing policy saved.';
		} catch {
			if (version !== generation) return;
			mustReload = true;
			error =
				'The save was not confirmed. Your draft is kept here. Reload the policy before trying again.';
		} finally {
			if (version === generation) saving = false;
		}
	}
</script>

<section class="space-y-4 rounded-md border border-border bg-card p-4" aria-labelledby="{id}-title">
	<h3 id="{id}-title" class="text-base font-semibold">Guest sharing policy</h3>
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	{#if status}<p role="status" class="text-sm text-primary-soft">{status}</p>{/if}
	{#if loading}
		<p role="status" class="text-sm text-muted-foreground">Loading sharing policy...</p>
	{:else if policy}
		<form class="space-y-4" onsubmit={(event) => void save(event)}>
			<label class="flex min-h-11 items-center gap-3 text-sm"
				><input
					type="checkbox"
					bind:checked={enabled}
					disabled={saving}
					class="size-4 accent-primary"
				/>Enable guest sharing</label
			>
			<label class="block space-y-2 text-sm" for="{id}-origin"
				><span>External HTTPS origin</span><Input
					id="{id}-origin"
					bind:value={origin}
					placeholder="https://recorder.example"
					autocomplete="off"
					spellcheck="false"
					class="min-h-11"
					disabled={saving}
				/></label
			>
			<p class="text-xs text-muted-foreground">
				Use the reachable HTTPS origin without a path or trailing slash. Recipients must be able to
				reach it. KeepPeek does not provide a tunnel or relay, or verify external reachability.
			</p>
			<p class="text-sm text-muted-foreground">
				Changing this policy ends active guest sessions. Disabling it prevents guest playback and
				new links.
			</p>
			{#if !policy.available}<p class="text-sm text-muted-foreground">
					{policy.unavailableReason || 'Guest sharing is currently unavailable.'}
				</p>{/if}
			<Button type="submit" class="min-h-11" disabled={saving || mustReload}
				>{saving ? 'Saving sharing policy...' : 'Save sharing policy'}</Button
			>
		</form>
	{/if}
	<Button
		variant="outline"
		class="min-h-11"
		onclick={() => void load()}
		disabled={loading || saving}>Reload policy</Button
	>
</section>
