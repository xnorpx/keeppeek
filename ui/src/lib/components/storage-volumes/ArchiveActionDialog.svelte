<script lang="ts">
	import type { ArchiveJob } from '$lib/proto/webrtc_pb';
	import { Button } from '$lib/components/ui/button/index.js';
	let {
		request,
		busy,
		error,
		onconfirm,
		onclose
	}: {
		request: { kind: 'evict' | 'restore'; job: ArchiveJob } | null;
		busy: boolean;
		error: string;
		onconfirm: () => void;
		onclose: () => void;
	} = $props();
	const id = $props.id();
	let dialog = $state<HTMLDialogElement>();
	let cancel = $state<HTMLButtonElement | null>(null);
	let evicting = $derived(request?.kind === 'evict');
	$effect(() => {
		if (!dialog || !request) return;
		const element = dialog;
		element.showModal();
		cancel?.focus();
		return () => element.close();
	});
</script>

<dialog
	bind:this={dialog}
	aria-labelledby={`${id}-title`}
	aria-describedby={`${id}-description`}
	{onclose}
	oncancel={(event) => {
		if (busy) event.preventDefault();
	}}
	onkeydown={(event) => {
		if (event.key === 'Escape') event.stopPropagation();
	}}
	class="m-auto max-h-[85dvh] w-[calc(100%-2rem)] max-w-lg space-y-5 overflow-auto rounded-md border border-hairline-strong bg-surface p-6 text-foreground shadow-xl backdrop:bg-black/55"
>
	<h2 id={`${id}-title`} class="text-lg-plus font-semibold">
		{evicting ? 'Evict primary copy?' : 'Restore primary copy?'}
	</h2>
	<p id={`${id}-description`} class="text-sm leading-relaxed text-text-muted">
		{evicting
			? 'Remove this primary file after the archive is verified again. Its timeline and archive copy remain available. Archive playback may start and seek more slowly.'
			: 'Restore this verified archive copy to its configured primary destination. This queues a background transfer and retains the archive copy.'}
	</p>
	{#if request}<dl class="space-y-2 rounded-sm border border-hairline bg-raised p-3 text-sm">
			<div>
				<dt class="text-xs text-text-muted">Object</dt>
				<dd class="font-mono break-all">{request.job.objectId}</dd>
			</div>
			<div>
				<dt class="text-xs text-text-muted">Archive target</dt>
				<dd class="font-mono break-all">{request.job.targetId}</dd>
			</div>
			<div>
				<dt class="text-xs text-text-muted">Size</dt>
				<dd class="font-mono">{request.job.bytes.toString()} bytes</dd>
			</div>
		</dl>{/if}
	<p class="text-xs text-text-muted">
		{evicting
			? 'Holds and active readers can block eviction. Primary retirement cannot be cancelled once its durable intent starts.'
			: 'Cancellation cleans up partial restore data before this job can retry.'}
	</p>
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	<div class="flex flex-wrap justify-end gap-3">
		<Button
			bind:ref={cancel}
			type="button"
			variant="outline"
			class="h-11"
			disabled={busy}
			onclick={onclose}>Cancel</Button
		>
		<Button type="button" class="h-11" disabled={busy} onclick={onconfirm}
			>{evicting ? 'Evict primary' : 'Restore primary'}</Button
		>
	</div>
</dialog>
