<script lang="ts">
	import { onMount } from 'svelte';
	import { Portal } from 'bits-ui';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import type { SharingControlClient } from '$lib/control-client-sharing';
	import { ShareGrantStatus, type ShareGrant } from '$lib/proto/webrtc_pb';
	import { shareStatus, shareTime } from '$lib/sharing';

	type Selection = { kind: 'grant' | 'artifact' | 'source'; grant: ShareGrant };
	let {
		controller
	}: { controller: Pick<SharingControlClient, 'list' | 'revoke' | 'revokeEvidence'> } = $props();
	const id = $props.id();
	// ponytail: Keep only the current 50-record page; use the server cursor for older grants.
	let grants = $state.raw<ShareGrant[]>([]);
	let cursor = $state<string | undefined>();
	let nextCursor = $state<string | undefined>();
	let allCreators = $state(false);
	let loading = $state(true);
	let mutating = $state(false);
	let mustReload = $state(false);
	let selection = $state.raw<Selection | null>(null);
	let dialog = $state<HTMLDialogElement | null>(null);
	let reason = $state('No longer needed');
	let error = $state<string | null>(null);
	let status = $state<string | null>(null);
	let generation = 0;

	onMount(() => {
		void load();
		return () => {
			generation += 1;
		};
	});
	$effect(() => {
		const element = dialog;
		if (!element) return;
		element.showModal();
		element.querySelector<HTMLButtonElement>('[data-guest-revoke-cancel]')?.focus();
		return () => element.close();
	});

	async function load(afterGrantId?: string): Promise<void> {
		const version = ++generation;
		loading = true;
		error = null;
		status = null;
		cursor = afterGrantId;
		grants = [];
		nextCursor = undefined;
		try {
			const result = await controller.list({ pageSize: 50, afterGrantId, allCreators });
			if (version !== generation) return;
			grants = result.grants;
			nextCursor = result.nextAfterGrantId;
			mustReload = false;
		} catch {
			if (version === generation) error = 'Guest links could not be loaded. Reload to try again.';
		} finally {
			if (version === generation) loading = false;
		}
	}

	function begin(kind: Selection['kind'], grant: ShareGrant): void {
		if (mutating || mustReload) return;
		selection = { kind, grant };
		reason = 'No longer needed';
		error = null;
	}

	function closeConfirmation(): void {
		selection = null;
		error = null;
	}

	async function confirm(): Promise<void> {
		if (!selection || mutating || mustReload) return;
		const selected = selection;
		const version = generation;
		if (!reason.trim() || new TextEncoder().encode(reason).byteLength > 256) {
			error = 'Enter a shorter revocation reason.';
			return;
		}
		mutating = true;
		error = null;
		try {
			if (selected.kind === 'grant') {
				const updated = await controller.revoke(selected.grant, reason);
				if (version !== generation) return;
				grants = grants.map((grant) => (grant.grantId === updated.grantId ? updated : grant));
				status = 'Guest access revoked.';
			} else {
				const evidence = selected.grant.evidence;
				if (!evidence) throw new Error('Evidence is unavailable.');
				const count = await controller.revokeEvidence(
					selected.kind === 'artifact'
						? { artifactId: evidence.artifactId }
						: { sourceIdentity: evidence.sourceIdentity },
					reason
				);
				if (version !== generation) return;
				mustReload = true;
				status = `Revoked ${count} guest links. Reload the list to see current grants.`;
			}
			selection = null;
		} catch {
			if (version !== generation) return;
			mustReload = true;
			error = 'Revocation could not be confirmed. Cancel and reload the list before trying again.';
		} finally {
			if (version === generation) mutating = false;
		}
	}
</script>

<section class="space-y-4" aria-labelledby="{id}-title">
	<div class="flex flex-wrap items-center justify-between gap-3">
		<h3 id="{id}-title" class="text-base font-semibold">Guest links</h3>
		<Button
			variant="outline"
			class="min-h-11"
			onclick={() => void load(cursor)}
			disabled={loading || mutating}>Reload guest links</Button
		>
	</div>
	<p class="text-sm text-muted-foreground">
		These records contain no usable link secret. Revocation stops current guest access. Shared
		evidence remains subject to privacy, deletion, and retention.
	</p>
	<label class="flex min-h-11 items-center gap-3 text-sm"
		><input
			type="checkbox"
			bind:checked={allCreators}
			onchange={() => void load()}
			disabled={loading || mutating}
			class="size-4 accent-primary"
		/>Show all creators</label
	>
	{#if error && !selection}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	{#if status}<p role="status" class="text-sm text-primary-soft">{status}</p>{/if}
	{#if mutating && !selection}<p role="status" class="text-sm text-muted-foreground">
			Revocation is still being checked. The list will update when the request finishes.
		</p>{/if}
	{#if loading}
		<p role="status" class="text-sm text-muted-foreground">Loading guest links...</p>
	{:else if grants.length === 0 && !error}
		<p class="text-sm text-muted-foreground">No guest links on this page.</p>
	{:else}
		<ul class="space-y-3">
			{#each grants as grant (grant.grantId)}
				<li class="space-y-3 rounded-md border border-border bg-card p-4">
					<div class="flex flex-wrap items-start justify-between gap-3">
						<div class="min-w-0 flex-1">
							<h4 class="font-semibold break-words">{grant.displayName}</h4>
							<p class="mt-1 text-sm text-muted-foreground">{shareStatus(grant.status)}</p>
						</div>
						<Button
							variant="outline"
							class="min-h-11"
							disabled={mutating || mustReload || grant.status === ShareGrantStatus.REVOKED}
							onclick={() => begin('grant', grant)}>Revoke link</Button
						>
					</div>
					<p class="font-mono text-xs break-all text-muted-foreground">Reference {grant.grantId}</p>
					<p class="text-sm text-muted-foreground">Expires {shareTime(grant.expiresAtMs)}</p>
					{#if grant.evidence}<p class="font-mono text-xs text-muted-foreground">
							{shareTime(grant.evidence.startMs)} - {shareTime(grant.evidence.endMs)}
						</p>{/if}
					<p class="text-sm">
						Views {grant.views} / {grant.maxViews ?? 'unlimited'}; downloads {grant.downloads} / {grant.maxDownloads ??
							'unlimited'}
					</p>
					<p class="text-xs text-muted-foreground">
						{grant.allowStream ? 'Playback allowed' : 'No playback'} / {grant.allowDownload
							? 'Download allowed'
							: 'No download'}
					</p>
					{#if grant.note}<p class="text-sm break-words text-muted-foreground">{grant.note}</p>{/if}
					<details>
						<summary class="min-h-11 cursor-pointer py-3 text-sm text-primary-soft"
							>Audit and evidence revocation</summary
						>
						<div class="space-y-3 pt-2">
							<p class="font-mono text-xs break-all text-muted-foreground">
								Creator {grant.creatorId}; created {shareTime(grant.createdAtMs)}
							</p>
							{#if grant.lastAccessAtMs !== undefined}<p class="text-xs text-muted-foreground">
									Last access {shareTime(grant.lastAccessAtMs)}
								</p>{/if}
							{#if grant.revokeReason}<p class="text-sm text-muted-foreground">
									Revocation reason: {grant.revokeReason}
								</p>{/if}
							{#if grant.evidence}
								<div class="flex flex-wrap gap-2">
									<Button
										variant="outline"
										class="min-h-11 whitespace-normal"
										disabled={mutating || mustReload}
										onclick={() => begin('artifact', grant)}>Revoke all links to this clip</Button
									>
									<Button
										variant="outline"
										class="min-h-11 whitespace-normal"
										disabled={mutating || mustReload}
										onclick={() => begin('source', grant)}>Revoke all links to this camera</Button
									>
								</div>
							{/if}
						</div>
					</details>
				</li>
			{/each}
		</ul>
	{/if}
	<div class="flex flex-wrap gap-2">
		{#if cursor}<Button
				variant="outline"
				class="min-h-11"
				disabled={loading || mutating}
				onclick={() => void load()}>Back to first page</Button
			>{/if}
		{#if nextCursor}<Button
				variant="outline"
				class="min-h-11"
				disabled={loading || mutating}
				onclick={() => void load(nextCursor)}>Next page</Button
			>{/if}
	</div>
</section>

{#if selection}
	<Portal>
		<dialog
			bind:this={dialog}
			aria-labelledby="{id}-confirm-title"
			class="m-auto max-h-[85dvh] w-[calc(100%-2rem)] max-w-lg space-y-4 overflow-auto rounded-md border border-border bg-background p-5 text-foreground shadow-xl backdrop:bg-black/55"
			oncancel={(event) => {
				event.preventDefault();
				closeConfirmation();
			}}
		>
			<h3 id="{id}-confirm-title" class="text-lg font-semibold">Revoke guest access</h3>
			<p class="text-sm break-words">{selection.grant.displayName}</p>
			<p class="text-sm text-muted-foreground">
				{selection.kind === 'grant'
					? 'This ends access through this guest link, including active playback.'
					: selection.kind === 'artifact'
						? 'This revokes every active guest link to this immutable clip, including links created by other Administrators.'
						: 'This revokes every active guest link to this camera identity, including other clips and creators.'}
			</p>
			<label class="block space-y-2 text-sm" for="{id}-reason"
				><span>Reason</span><Input
					id="{id}-reason"
					bind:value={reason}
					maxlength={256}
					disabled={mutating}
					class="min-h-11"
				/></label
			>
			{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
			<div class="flex flex-wrap justify-end gap-2">
				<Button
					data-guest-revoke-cancel
					variant="outline"
					class="min-h-11"
					onclick={closeConfirmation}>Cancel</Button
				><Button
					variant="destructive"
					class="min-h-11"
					disabled={mutating || mustReload}
					onclick={() => void confirm()}>{mutating ? 'Revoking...' : 'Confirm revocation'}</Button
				>
			</div>
		</dialog>
	</Portal>
{/if}
