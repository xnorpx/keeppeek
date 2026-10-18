<script lang="ts">
	import { untrack } from 'svelte';
	import { Portal } from 'bits-ui';
	import type { RecordedAudioTiming } from '$lib/proto/webrtc_pb';
	import { parseRecordedAudioOffset } from '$lib/recorded-audio';
	import { Button } from './ui/button/index.js';
	import RecordedAudioTimingDetails from './RecordedAudioTimingDetails.svelte';

	type Props = {
		timing?: RecordedAudioTiming;
		name: string;
		disabled?: boolean;
		onapply: (offsetMs: number | null) => Promise<void>;
		onclose: () => void;
	};
	let { timing, name, disabled = false, onapply, onclose }: Props = $props();
	let dialog = $state<HTMLDialogElement>();
	let draft = $state<number | undefined>(untrack(() => timing?.appliedOffsetMs ?? 0));
	let error = $state<string | null>(null);
	let busy = $state(false);
	let operation = 0;
	let largeOffset = $derived(Math.abs(draft ?? 0) > 1_000);

	$effect(() => {
		const element = dialog;
		if (!element) return;
		element.showModal();
		return () => {
			operation += 1;
			element.close();
		};
	});

	async function apply(reset = false): Promise<void> {
		if (busy || disabled || !timing) return;
		const version = ++operation;
		busy = true;
		error = null;
		try {
			await onapply(reset ? null : parseRecordedAudioOffset(String(draft ?? '')));
			if (version === operation && reset) draft = timing.configuredOffsetMs;
		} catch (cause) {
			if (version === operation)
				error = cause instanceof Error ? cause.message : 'Audio preview could not apply.';
		} finally {
			if (version === operation) busy = false;
		}
	}
</script>

<Portal>
	<dialog
		bind:this={dialog}
		aria-labelledby="recorded-audio-preview-title"
		class="fixed inset-x-0 top-auto bottom-0 m-0 max-h-[90dvh] w-full max-w-none overflow-auto rounded-t-md border border-hairline bg-surface p-4 text-foreground shadow-xl backdrop:bg-black/55 sm:inset-0 sm:m-auto sm:max-w-[26rem] sm:rounded-md"
		oncancel={(event) => {
			event.preventDefault();
			onclose();
		}}
	>
		<header class="mb-3 flex items-center justify-between gap-3">
			<h2 id="recorded-audio-preview-title" class="text-lg font-semibold">
				Recorded audio preview
			</h2>
			<Button variant="ghost" class="min-h-11 min-w-11" onclick={onclose}>Close</Button>
		</header>
		<p class="mb-4 text-xs text-text-muted">{name}</p>
		<RecordedAudioTimingDetails {timing} />
		{#if timing}
			<form
				class="mt-4 grid gap-3"
				onsubmit={(event) => {
					event.preventDefault();
					void apply();
				}}
			>
				<label class="grid gap-1 text-sm"
					>Preview offset (ms)
					<input
						type="number"
						min="-5000"
						max="5000"
						step="1"
						bind:value={draft}
						class="min-h-11 w-full rounded-sm border border-input bg-background px-3 font-mono"
						disabled={busy || disabled}
					/></label
				>
				<p class="text-xs text-text-muted">
					Positive delays audio; negative plays it earlier. Whole milliseconds from -5000 to +5000.
				</p>
				{#if largeOffset}<p role="status" class="text-xs text-activity">
						More than 1000 ms can create a noticeable lead or delay. Check a clear sound and visible
						action.
					</p>{/if}
				{#if error}<p role="alert" class="text-sm text-destructive">
						{error} Your draft is retained.
					</p>{/if}
				<div class="flex flex-wrap gap-2">
					<Button
						type="button"
						variant="outline"
						class="min-h-11 flex-1"
						disabled={busy || disabled}
						onclick={() => void apply(true)}>Reset preview</Button
					>
					<Button type="submit" class="min-h-11 flex-1" disabled={busy || disabled}
						>{busy ? 'Applying preview' : 'Apply preview'}</Button
					>
				</div>
			</form>
		{/if}
		<p class="mt-3 text-xs text-text-muted">
			Preview is only for this open review. Reset uses saved settings.
		</p>
		<p class="mt-2 text-xs text-text-muted">New exports use saved settings.</p>
		<p class="mt-2 text-xs text-text-muted">
			Save a permanent change in Camera / Audio. Live correction is unavailable.
		</p>
	</dialog>
</Portal>
