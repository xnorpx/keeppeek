<script lang="ts">
	import { Button } from '$lib/components/ui/button/index.js';
	import CheckIcon from '@lucide/svelte/icons/check';
	import CopyIcon from '@lucide/svelte/icons/copy';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import EyeOffIcon from '@lucide/svelte/icons/eye-off';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';

	type Props = {
		pending: boolean;
		onclaim: () => Promise<string>;
		onclaimed?: () => void;
	};

	let { pending, onclaim, onclaimed }: Props = $props();
	let accessKey = $state<string | null>(null);
	let busy = $state(false);
	let copied = $state(false);
	let error = $state<string | null>(null);

	async function claim(): Promise<void> {
		busy = true;
		error = null;
		try {
			accessKey = await onclaim();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Initial access key could not be retrieved.';
		} finally {
			busy = false;
		}
	}

	async function copy(): Promise<void> {
		if (!accessKey) return;
		try {
			await navigator.clipboard.writeText(accessKey);
			copied = true;
		} catch {
			error = 'Clipboard access is unavailable.';
		}
	}

	function download(): void {
		if (!accessKey) return;
		const url = URL.createObjectURL(new Blob([`${accessKey}\n`], { type: 'text/plain' }));
		const anchor = document.createElement('a');
		anchor.href = url;
		anchor.download = 'keeppeek-initial-administrator-key.txt';
		anchor.click();
		URL.revokeObjectURL(url);
	}

	function hide(): void {
		accessKey = null;
		copied = false;
		onclaimed?.();
	}
</script>

<section class="border-y border-hairline bg-surface p-4" aria-labelledby="initial-access-heading">
	<div class="flex items-start gap-3">
		<span class="grid size-9 shrink-0 place-items-center rounded-sm bg-raised text-primary-soft">
			{#if pending || accessKey}<KeyRoundIcon class="size-4" />{:else}<CheckIcon
					class="size-4 text-healthy"
				/>{/if}
		</span>
		<div class="min-w-0 flex-1">
			<h2 id="initial-access-heading" class="text-sm font-semibold">
				Remote Administrator credential
			</h2>
			<p class="mt-1 text-xs leading-5 text-text-muted">
				{#if accessKey}
					Save this key now. It cannot be retrieved again after it is hidden.
				{:else if pending}
					Remote access is not ready until the initial key is retrieved and stored safely.
				{:else}
					The initial key was retrieved. Named credentials can be managed in Settings.
				{/if}
			</p>
		</div>
	</div>

	{#if accessKey}
		<code
			class="mt-4 block overflow-x-auto border-y border-hairline bg-background px-3 py-2 font-mono text-xs select-all"
			>{accessKey}</code
		>
		<div class="mt-3 flex flex-wrap gap-2">
			<Button size="sm" onclick={() => void copy()}>
				{#if copied}<CheckIcon class="size-3.5" /> Copied{:else}<CopyIcon class="size-3.5" /> Copy{/if}
			</Button>
			<Button variant="outline" size="sm" onclick={download}>
				<DownloadIcon class="size-3.5" /> Download
			</Button>
			<Button variant="ghost" size="sm" onclick={hide}>
				<EyeOffIcon class="size-3.5" /> Hide permanently
			</Button>
		</div>
	{:else if pending}
		<Button class="mt-4" size="sm" onclick={() => void claim()} disabled={busy}>
			<KeyRoundIcon class="size-3.5" />
			{busy ? 'Retrieving' : 'Retrieve initial key'}
		</Button>
	{/if}

	{#if error}<p class="mt-3 text-xs text-destructive" role="alert">{error}</p>{/if}
</section>
