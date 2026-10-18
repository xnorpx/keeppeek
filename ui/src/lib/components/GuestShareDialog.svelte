<script lang="ts">
	import { onMount } from 'svelte';
	import { Portal } from 'bits-ui';
	import { resolve } from '$app/paths';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import type { SharingControlClient } from '$lib/control-client-sharing';
	import type { ShareEvidence, SharePolicy } from '$lib/proto/webrtc_pb';
	import { shareBytes, shareDateInput, shareExpiry, shareQuota, shareTime } from '$lib/sharing';
	import XIcon from '@lucide/svelte/icons/x';
	import CopyIcon from '@lucide/svelte/icons/copy';

	type Stage = 'loading' | 'preview' | 'creating' | 'created' | 'unconfirmed' | 'error';
	type Props = {
		controller: Pick<SharingControlClient, 'getPolicy' | 'previewExport' | 'create'>;
		exportJobId: string;
		onclose: () => void;
	};
	let { controller, exportJobId, onclose }: Props = $props();
	const id = $props.id();
	let dialog = $state<HTMLDialogElement | null>(null);
	let stage = $state<Stage>('loading');
	let evidence = $state.raw<ShareEvidence | null>(null);
	let policy = $state.raw<SharePolicy | null>(null);
	let displayName = $state('');
	let note = $state('');
	let expiry = $state('');
	let allowStream = $state(true);
	let allowDownload = $state(false);
	let maxViews = $state('');
	let maxDownloads = $state('');
	let error = $state<string | null>(null);
	let url = $state<string | null>(null);
	let copied = $state(false);
	let requestId = $state<string | null>(null);
	let generation = 0;
	let creationAvailable = $derived(
		policy?.settings?.enabled && policy.available && policy.settings.publicOrigin
	);

	$effect(() => {
		const element = dialog;
		if (!element) return;
		element.showModal();
		return () => element.close();
	});

	onMount(() => {
		const leave = () => close();
		window.addEventListener('pagehide', leave);
		void load();
		return () => {
			window.removeEventListener('pagehide', leave);
			generation += 1;
			url = null;
		};
	});

	async function load(): Promise<void> {
		const version = ++generation;
		stage = 'loading';
		error = null;
		try {
			const [nextPolicy, nextEvidence] = await Promise.all([
				controller.getPolicy(),
				controller.previewExport(exportJobId)
			]);
			if (version !== generation) return;
			policy = nextPolicy;
			evidence = nextEvidence;
			displayName = nextEvidence.cameraName || 'Shared footage';
			expiry = shareDateInput(
				Math.min(Date.now() + 3_600_000, Number(nextEvidence.availableUntilMs))
			);
			stage = 'preview';
		} catch {
			if (version !== generation) return;
			stage = 'error';
			error =
				'This export could not be previewed for sharing. Check access and file expiry, and use a new creator-owned export of at most 2 minutes and 512 MiB. Then reload.';
		}
	}

	async function createLink(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (stage !== 'preview' || !creationAvailable || !evidence) return;
		error = null;
		let limits: { expiresAtMs: bigint; maxViews?: number; maxDownloads?: number };
		try {
			limits = {
				expiresAtMs: shareExpiry(expiry, evidence.availableUntilMs),
				maxViews: allowStream ? shareQuota(maxViews) : undefined,
				maxDownloads: allowDownload ? shareQuota(maxDownloads) : undefined
			};
			if (
				!displayName.trim() ||
				new TextEncoder().encode(displayName).byteLength > 128 ||
				new TextEncoder().encode(note).byteLength > 512
			)
				throw new Error('Use a shorter display name or note. A display name is required.');
			if (!allowStream && !allowDownload)
				throw new Error('Allow playback or download to continue.');
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Review the link settings.';
			return;
		}
		const version = generation;
		requestId = crypto.randomUUID();
		stage = 'creating';
		try {
			const issued = await controller.create(
				{
					requestId,
					exportJobId: evidence.exportJobId,
					artifactId: evidence.artifactId,
					expectedSha256: evidence.sha256,
					displayName,
					note,
					allowStream,
					allowDownload,
					...limits
				},
				creationAvailable
			);
			if (version !== generation) return;
			url = issued.url;
			stage = 'created';
		} catch {
			if (version !== generation) return;
			stage = 'unconfirmed';
			error =
				'Creation was not confirmed. The link may exist. Review and revoke that grant before creating another.';
		}
	}

	async function copyLink(): Promise<void> {
		if (!url) return;
		const version = generation;
		error = null;
		try {
			await navigator.clipboard.writeText(url);
			if (version === generation) copied = true;
		} catch {
			if (version === generation)
				error = 'The clipboard is unavailable. Select and copy the link below.';
		}
	}

	function close(): void {
		generation += 1;
		url = null;
		dialog?.close();
		onclose();
	}
</script>

<Portal>
	<dialog
		bind:this={dialog}
		class="m-auto max-h-[90dvh] w-[calc(100%-2rem)] max-w-2xl overflow-auto rounded-md border border-border bg-background p-0 text-foreground shadow-xl backdrop:bg-black/55"
		aria-labelledby="{id}-title"
		oncancel={(event) => {
			event.preventDefault();
			close();
		}}
	>
		<header class="flex items-center gap-3 border-b border-border p-4">
			<div class="min-w-0 flex-1">
				<h2 id="{id}-title" class="text-lg font-semibold">Create guest link</h2>
				<p class="mt-1 text-sm text-muted-foreground">Share one completed evidence file.</p>
			</div>
			<Button
				variant="ghost"
				size="icon"
				class="size-11"
				onclick={close}
				aria-label="Close guest sharing"><XIcon class="size-4" /></Button
			>
		</header>
		<div class="space-y-5 p-4 sm:p-6">
			{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
			{#if stage === 'loading'}
				<p role="status" class="text-sm text-muted-foreground">
					Checking the immutable export and sharing policy...
				</p>
			{:else if stage === 'error'}
				<Button variant="outline" class="min-h-11" onclick={() => void load()}
					>Reload preview</Button
				>
			{:else if stage === 'created' && url}
				<div class="space-y-4">
					<h3 class="text-base font-semibold">Copy this link now</h3>
					<p class="text-sm text-muted-foreground">
						Anyone with this link can use the access you selected. KeepPeek will not show it again
						after you close this dialog.
					</p>
					<label for="{id}-url" class="block text-sm font-medium">One-time guest link</label>
					<textarea
						id="{id}-url"
						value={url}
						readonly
						rows="3"
						spellcheck="false"
						autocomplete="off"
						class="w-full resize-none rounded-sm border border-border bg-card p-3 font-mono text-xs break-all focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					></textarea>
					<Button class="min-h-11" onclick={() => void copyLink()}
						><CopyIcon class="size-4" />Copy link</Button
					>
					{#if copied}<p role="status" class="text-sm text-primary-soft">Link copied.</p>{/if}
				</div>
			{:else if stage === 'unconfirmed'}
				<p class="text-sm text-muted-foreground">
					Find the grant with this reference in Guest links. The list can revoke access; it cannot
					recover the secret.
				</p>
				<p class="font-mono text-xs break-all">{requestId}</p>
				<a
					href="{resolve('/settings')}#access"
					onclick={close}
					class="inline-flex min-h-11 items-center text-sm text-primary-soft underline"
					>Review guest links</a
				>
			{:else if evidence && policy}
				<section
					class="space-y-3 rounded-sm border border-border bg-card p-4"
					aria-label="Evidence preview"
				>
					<h3 class="text-base font-semibold break-words">
						{evidence.cameraName || 'Shared footage'}
					</h3>
					<dl class="grid gap-3 text-sm sm:grid-cols-2">
						<div>
							<dt class="text-muted-foreground">File starts</dt>
							<dd class="mt-1 font-mono text-xs">{shareTime(evidence.startMs)}</dd>
						</div>
						<div>
							<dt class="text-muted-foreground">File ends</dt>
							<dd class="mt-1 font-mono text-xs">{shareTime(evidence.endMs)}</dd>
						</div>
						<div>
							<dt class="text-muted-foreground">Evidence</dt>
							<dd>
								{Number(evidence.endMs - evidence.startMs) / 1_000} s / {shareBytes(evidence.bytes)}
							</dd>
						</div>
						<div>
							<dt class="text-muted-foreground">File expiry</dt>
							<dd class="mt-1 font-mono text-xs">{shareTime(evidence.availableUntilMs)}</dd>
						</div>
					</dl>
					<p class="text-sm text-muted-foreground">
						This shares the complete MP4 with the bounds shown. It may start before the range
						originally requested to include a preceding keyframe.
					</p>
					{#if evidence.hasGaps}<p role="status" class="text-sm text-activity">
							The file contains gaps in recorded coverage.
						</p>{/if}
					<p class="text-xs text-muted-foreground">
						Sharing does not keep the file longer. Deletion, privacy, or retention can make it
						unavailable sooner.
					</p>
				</section>
				<section
					class="space-y-2 rounded-sm border border-border p-4"
					aria-label="External guest destination"
				>
					<p class="text-sm font-medium">External HTTPS destination</p>
					<p class="font-mono text-xs break-all">
						{policy.settings?.publicOrigin
							? `${policy.settings.publicOrigin}/share`
							: 'No external origin configured'}
					</p>
					<p class="text-xs text-muted-foreground">
						Recipients must be able to reach this address. KeepPeek does not create a tunnel or
						relay, or verify external reachability.
					</p>
				</section>
				{#if !creationAvailable}
					<p role="status" class="text-sm text-muted-foreground">
						{policy.unavailableReason ||
							'Guest sharing is disabled or unavailable. An Administrator can review the sharing policy in Settings Access.'}
					</p>
				{:else}
					<form class="space-y-4" onsubmit={(event) => void createLink(event)}>
						<label class="block space-y-2 text-sm" for="{id}-name"
							><span>Display name</span><Input
								id="{id}-name"
								bind:value={displayName}
								maxlength={128}
								required
								class="min-h-11"
								disabled={stage === 'creating'}
							/></label
						>
						<label class="block space-y-2 text-sm" for="{id}-note"
							><span>Private note (optional)</span><Input
								id="{id}-note"
								bind:value={note}
								maxlength={512}
								class="min-h-11"
								disabled={stage === 'creating'}
							/></label
						>
						<label class="block space-y-2 text-sm" for="{id}-expiry"
							><span>Expires (UTC)</span><Input
								id="{id}-expiry"
								type="datetime-local"
								step="1"
								bind:value={expiry}
								required
								class="min-h-11"
								disabled={stage === 'creating'}
							/></label
						>
						<fieldset class="flex flex-wrap gap-x-6 gap-y-2" disabled={stage === 'creating'}>
							<legend class="mb-2 text-sm font-medium">Guest access</legend>
							<label class="flex min-h-11 items-center gap-2 text-sm"
								><input
									type="checkbox"
									bind:checked={allowStream}
									class="size-4 accent-primary"
								/>Allow playback</label
							>
							<label class="flex min-h-11 items-center gap-2 text-sm"
								><input
									type="checkbox"
									bind:checked={allowDownload}
									class="size-4 accent-primary"
								/>Allow download</label
							>
						</fieldset>
						{#if !allowStream && !allowDownload}<p
								role="status"
								class="text-sm text-muted-foreground"
							>
								Allow playback or download to continue.
							</p>{/if}
						<div class="grid gap-4 sm:grid-cols-2">
							<label class="block space-y-2 text-sm" for="{id}-views"
								><span>Maximum views</span><Input
									id="{id}-views"
									inputmode="numeric"
									pattern="[0-9]*"
									bind:value={maxViews}
									placeholder="Unlimited"
									class="min-h-11"
									disabled={!allowStream || stage === 'creating'}
								/></label
							>
							<label class="block space-y-2 text-sm" for="{id}-downloads"
								><span>Maximum downloads</span><Input
									id="{id}-downloads"
									inputmode="numeric"
									pattern="[0-9]*"
									bind:value={maxDownloads}
									placeholder="Unlimited"
									class="min-h-11"
									disabled={!allowDownload || stage === 'creating'}
								/></label
							>
						</div>
						<p class="text-xs text-muted-foreground">
							Blank limits are unlimited. A view counts when playback first delivers media; seeks in
							that session reuse the view. Downloads count separately.
						</p>
						<Button
							type="submit"
							class="min-h-11 w-full sm:w-auto"
							disabled={stage === 'creating' || (!allowStream && !allowDownload)}
							>{stage === 'creating' ? 'Creating guest link...' : 'Create guest link'}</Button
						>
					</form>
				{/if}
			{/if}
		</div>
		<footer class="flex justify-end border-t border-border p-4">
			<Button variant="outline" class="min-h-11" onclick={close}>Close</Button>
		</footer>
	</dialog>
</Portal>
