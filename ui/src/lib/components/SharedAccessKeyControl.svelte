<script lang="ts">
	import { Button } from '$lib/components/ui/button/index.js';
	import CopyIcon from '@lucide/svelte/icons/copy';
	import EyeIcon from '@lucide/svelte/icons/eye';
	import EyeOffIcon from '@lucide/svelte/icons/eye-off';
	import RotateCwIcon from '@lucide/svelte/icons/rotate-cw';

	type Props = {
		onreveal: () => Promise<string>;
		onrotate: () => Promise<string>;
		compact?: boolean;
	};

	let { onreveal, onrotate, compact = false }: Props = $props();

	let accessKey = $state<string | null>(null);
	let busy = $state(false);
	let confirmRotation = $state(false);
	let error = $state<string | null>(null);
	let status = $state<string | null>(null);

	async function reveal(): Promise<void> {
		busy = true;
		error = null;
		status = null;
		confirmRotation = false;
		try {
			accessKey = await onreveal();
			status = 'Key revealed locally. Keep it private.';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Access key could not be revealed.';
		} finally {
			busy = false;
		}
	}

	async function rotate(): Promise<void> {
		if (!confirmRotation) {
			confirmRotation = true;
			error = null;
			status = 'Confirm rotation to revoke the previous key and its active remote sessions.';
			return;
		}
		busy = true;
		error = null;
		try {
			accessKey = await onrotate();
			status = 'Key rotated. Copy the replacement before leaving this page.';
			confirmRotation = false;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Access key could not be rotated.';
		} finally {
			busy = false;
		}
	}

	async function copyAccessKey(): Promise<void> {
		if (!accessKey) return;
		try {
			await navigator.clipboard.writeText(accessKey);
			status = 'Access key copied.';
			error = null;
		} catch {
			error = 'Clipboard access is unavailable. Select the key and copy it manually.';
		}
	}

	function hideAccessKey(): void {
		accessKey = null;
		confirmRotation = false;
		status = null;
		error = null;
	}
</script>

<div
	data-shared-access-key-control
	class={compact
		? 'space-y-3 rounded-sm border border-hairline bg-surface p-3'
		: 'space-y-3 rounded-sm border border-hairline-strong bg-raised/40 p-4'}
>
	<div class="flex flex-wrap items-start justify-between gap-3">
		<div class="max-w-lg min-w-0">
			<p class="text-sm font-semibold">Shared remote access key</p>
			<p class="mt-1 text-xs leading-5 text-text-muted">
				{compact
					? 'Token registry unavailable. This machine can manage the one shared key.'
					: 'Available only from this machine. Rotation immediately replaces the saved key and closes remote sessions authenticated with the previous value.'}
			</p>
		</div>
		<div class="flex shrink-0 flex-wrap gap-2">
			{#if accessKey}
				<Button variant="outline" size="sm" onclick={() => void copyAccessKey()} disabled={busy}>
					<CopyIcon class="size-3.5" /> Copy key
				</Button>
				<Button variant="outline" size="sm" onclick={hideAccessKey} disabled={busy}>
					<EyeOffIcon class="size-3.5" /> Hide
				</Button>
			{:else}
				<Button variant="outline" size="sm" onclick={() => void reveal()} disabled={busy}>
					<EyeIcon class="size-3.5" />
					{busy ? 'Revealing' : 'Reveal key'}
				</Button>
			{/if}
			<Button
				variant={confirmRotation ? 'destructive' : 'outline'}
				size="sm"
				onclick={() => void rotate()}
				disabled={busy}
			>
				<RotateCwIcon class="size-3.5" />
				{busy ? 'Rotating' : confirmRotation ? 'Confirm rotation' : 'Rotate key'}
			</Button>
		</div>
	</div>

	{#if accessKey}
		<code
			class="block overflow-x-auto border-y border-hairline bg-background px-3 py-2 font-mono text-xs text-foreground select-all"
			aria-label="Revealed shared remote access key">{accessKey}</code
		>
	{/if}
	{#if status || error}
		<p
			class="text-xs leading-5 {error ? 'text-destructive' : 'text-text-muted'}"
			aria-live="polite"
		>
			{error ?? status}
		</p>
	{/if}
</div>
