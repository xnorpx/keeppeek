<script lang="ts">
	import { onDestroy, tick } from 'svelte';
	import LinkIcon from '@lucide/svelte/icons/link';
	import CheckIcon from '@lucide/svelte/icons/check';
	import XIcon from '@lucide/svelte/icons/x';
	import { Button } from './ui/button/index.js';

	let { getLink, disabled = false }: { getLink: () => string | null; disabled?: boolean } =
		$props();
	const id = $props.id();
	let trigger: HTMLButtonElement | null = null;
	let dialog: HTMLDialogElement | null = null;
	let input: HTMLInputElement | null = null;
	let busy = $state(false);
	let copied = $state(false);
	let status = $state('');
	let fallbackLink = $state('');
	let mounted = true;
	let clipboardTimer: ReturnType<typeof setTimeout> | undefined;
	let statusTimer: ReturnType<typeof setTimeout> | undefined;

	onDestroy(() => {
		mounted = false;
		clearTimeout(clipboardTimer);
		clearTimeout(statusTimer);
	});

	async function copy(): Promise<void> {
		if (disabled || busy) return;
		clearTimeout(statusTimer);
		status = '';
		copied = false;
		let link: string | null;
		try {
			link = getLink();
		} catch {
			link = null;
		}
		if (!link) {
			status = 'A recording moment is not available to copy.';
			return;
		}
		busy = true;
		try {
			if (!navigator.clipboard?.writeText) throw new Error('Clipboard unavailable');
			const write = navigator.clipboard.writeText(link);
			await Promise.race([
				write,
				new Promise<never>((_resolve, reject) => {
					clipboardTimer = setTimeout(() => reject(new Error('Clipboard timed out')), 2500);
				})
			]);
			if (!mounted) return;
			copied = true;
			status = 'Recording link copied. Sign-in required.';
			statusTimer = setTimeout(() => {
				status = '';
				copied = false;
			}, 5000);
		} catch {
			if (!mounted) return;
			fallbackLink = link;
			status =
				'Clipboard access is unavailable. The recording link is selected for manual copying.';
			await tick();
			if (!mounted || !dialog || !input) return;
			dialog.showModal();
			input.focus({ preventScroll: true });
			input.select();
		} finally {
			clearTimeout(clipboardTimer);
			if (mounted) busy = false;
		}
	}

	function close(): void {
		dialog?.close();
	}
	function restoreFocus(): void {
		fallbackLink = '';
		trigger?.focus({ preventScroll: true });
	}
</script>

<div class="contents">
	<button
		bind:this={trigger}
		type="button"
		class="inline-flex size-11 shrink-0 items-center justify-center rounded-sm border border-hairline-strong bg-surface text-text-muted hover:bg-raised hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50 md:size-9"
		aria-label="Copy link to this moment (sign-in required)"
		title={copied
			? 'Recording link copied. Sign-in required.'
			: 'Copy link to this moment (sign-in required)'}
		aria-busy={busy}
		{disabled}
		onclick={() => void copy()}
	>
		{#if copied}<CheckIcon class="size-4" />{:else}<LinkIcon class="size-4" />{/if}
	</button>
	<span class="sr-only" role={status ? 'status' : undefined} aria-live="polite" aria-atomic="true"
		>{status}</span
	>
	<dialog
		bind:this={dialog}
		aria-labelledby={`${id}-title`}
		aria-describedby={`${id}-description`}
		onclose={restoreFocus}
		onkeydown={(event) => {
			if (event.key === 'Escape') event.stopPropagation();
		}}
		class="m-auto w-[min(32rem,calc(100%-2rem))] rounded-lg border border-hairline-strong bg-surface p-5 text-foreground shadow-lg backdrop:bg-black/50"
	>
		<div class="mb-3 flex items-center justify-between gap-3">
			<h2 id={`${id}-title`} class="text-base font-semibold">Copy recording link</h2>
			<Button
				variant="ghost"
				size="icon"
				class="size-11"
				aria-label="Close copy dialog"
				title="Close copy dialog"
				onclick={close}><XIcon class="size-4" /></Button
			>
		</div>
		<p id={`${id}-description`} class="mb-3 text-sm text-text-muted">
			Sign-in and camera access are required. Copy the selected link manually.
		</p>
		<label for={`${id}-link`} class="mb-1 block text-sm font-medium"
			>Authenticated recording link</label
		>
		<input
			bind:this={input}
			id={`${id}-link`}
			type="text"
			readonly
			value={fallbackLink}
			class="h-11 w-full min-w-0 rounded-sm border border-hairline-strong bg-ground px-3 text-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			onclick={(event) => event.currentTarget.select()}
		/>
	</dialog>
</div>
