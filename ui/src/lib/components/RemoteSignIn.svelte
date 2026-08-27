<script lang="ts">
	import type { AccessConnectionState } from '$lib/access';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';

	type Props = {
		state: AccessConnectionState;
		onsignin: (accessKey: string) => Promise<void>;
		onretry: () => Promise<void>;
	};

	let { state: accessState, onsignin, onretry }: Props = $props();
	let accessKey = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (busy || !accessKey.trim()) return;
		busy = true;
		error = null;
		try {
			await onsignin(accessKey);
			accessKey = '';
		} catch {
			accessKey = '';
			error = 'Sign-in failed. Check the access key and try again.';
		} finally {
			busy = false;
		}
	}

	async function retry(): Promise<void> {
		busy = true;
		error = null;
		try {
			await onretry();
		} catch {
			error = 'KeepPeek is still unavailable.';
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head>
	<title>Access · KeepPeek</title>
</svelte:head>

<div class="grid min-h-svh place-items-center bg-background px-5 py-10 text-foreground">
	<main class="w-full max-w-sm" aria-labelledby="access-gate-heading">
		<div class="mb-8 flex items-center gap-3">
			<span
				class="grid size-9 place-items-center rounded-sm bg-primary font-mono text-sm font-semibold text-primary-foreground"
				>K</span
			>
			<div>
				<p class="text-sm font-semibold">KeepPeek</p>
				<p class="text-xs text-text-muted">Protected recorder access</p>
			</div>
		</div>

		<section class="border-y border-hairline py-7">
			<div
				class="mb-5 flex size-10 items-center justify-center rounded-sm bg-raised text-primary-soft"
			>
				<KeyRoundIcon class="size-5" strokeWidth={1.75} />
			</div>
			{#if accessState.status === 'checking'}
				<h1 id="access-gate-heading" class="text-xl font-semibold">Checking local access</h1>
				<p class="mt-2 text-sm leading-6 text-text-muted" role="status">
					Establishing a protected session with this recorder.
				</p>
				<div class="mt-6 h-1 overflow-hidden bg-raised">
					<div class="h-full w-1/2 animate-pulse bg-primary"></div>
				</div>
			{:else if accessState.status === 'sign-in-required'}
				<h1 id="access-gate-heading" class="text-xl font-semibold">Remote sign-in</h1>
				<p class="mt-2 text-sm leading-6 text-text-muted">
					Use an Administrator or User access key issued by this recorder.
				</p>
				<form class="mt-6 space-y-4" onsubmit={(event) => void submit(event)}>
					<div class="space-y-2">
						<label for="remote-access-key" class="text-xs font-medium">Access key</label>
						<Input
							id="remote-access-key"
							type="password"
							bind:value={accessKey}
							autocomplete="off"
							autocapitalize="none"
							spellcheck="false"
							disabled={busy}
							required
						/>
					</div>
					<Button type="submit" class="w-full" disabled={busy || !accessKey.trim()}>
						<KeyRoundIcon class="size-4" />
						{busy ? 'Signing in' : 'Sign in'}
					</Button>
				</form>
			{:else}
				<h1 id="access-gate-heading" class="text-xl font-semibold">Session unavailable</h1>
				<p class="mt-2 text-sm leading-6 text-text-muted">
					{accessState.message ?? 'KeepPeek could not establish a protected session.'}
				</p>
				<Button variant="outline" class="mt-6" onclick={() => void retry()} disabled={busy}>
					<RefreshCwIcon class="size-4" />
					{busy ? 'Retrying' : 'Retry'}
				</Button>
			{/if}

			{#if accessState.message && accessState.status === 'sign-in-required'}
				<p class="mt-4 text-xs leading-5 text-destructive" role="alert">{accessState.message}</p>
			{/if}
			{#if error}
				<p class="mt-4 text-xs leading-5 text-destructive" role="alert">{error}</p>
			{/if}
		</section>
	</main>
</div>
