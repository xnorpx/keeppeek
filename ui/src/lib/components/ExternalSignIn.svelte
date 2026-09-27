<script lang="ts">
	import type { AccessConnectionState } from '$lib/access';
	import {
		ExternalAuthentication,
		authenticationError,
		submitProviderLogin,
		validReturnPath
	} from '$lib/external-authentication.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import RemoteSignIn from './RemoteSignIn.svelte';

	let {
		authentication,
		state: accessState,
		onsignin,
		onretry
	}: {
		authentication: ExternalAuthentication;
		state: AccessConnectionState;
		onsignin: (key: string) => Promise<void>;
		onretry: () => Promise<void>;
	} = $props();
	let busy = $state(false);
	let error = $state<string | null>(null);
	let accessKey = $state('');
	let session = $derived(authentication.session);
	let waiting = $derived(busy || authentication.busy || accessState.status === 'checking');

	async function retry(): Promise<void> {
		busy = true;
		error = null;
		try {
			await onretry();
		} catch (failure) {
			error = authenticationError(failure);
		} finally {
			busy = false;
		}
	}

	async function login(providerId: string): Promise<void> {
		busy = true;
		error = null;
		try {
			await authentication.discover();
			const current = authentication.session;
			if (current?.identity) {
				await onretry();
				return;
			}
			if (
				!current?.csrf_token ||
				!current.methods.some((method) => method.id === providerId && method.kind === 'oidc')
			) {
				throw new Error('Sign-in method is unavailable.');
			}
			const target = window.location.pathname + window.location.search;
			submitProviderLogin(providerId, current.csrf_token, validReturnPath(target) ? target : '/');
		} catch (failure) {
			error = authenticationError(failure);
		} finally {
			busy = false;
		}
	}

	async function bearerSignIn(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (waiting || !accessKey.trim()) return;
		busy = true;
		error = null;
		try {
			await onsignin(accessKey);
		} catch {
			error = 'Sign-in failed. Check the access key and try again.';
		} finally {
			accessKey = '';
			busy = false;
		}
	}
</script>

<svelte:head><title>Sign in · KeepPeek</title></svelte:head>

{#if session?.bearer_enabled && session.methods.length === 0}
	<RemoteSignIn state={accessState} {onsignin} {onretry} />
{:else}
	<div class="grid min-h-svh place-items-center bg-background px-5 py-10 text-foreground">
		<main
			class="w-full max-w-sm space-y-5"
			aria-labelledby="browser-sign-in-heading"
			aria-busy={waiting}
		>
			<p class="text-sm font-semibold">KeepPeek</p>
			<h1 id="browser-sign-in-heading" class="text-xl font-semibold">
				{waiting ? 'Checking access' : 'Sign in'}
			</h1>
			{#if waiting}
				<p class="text-sm text-muted-foreground" role="status">
					Connecting securely to your recorder.
				</p>
			{:else}
				{#each session?.methods ?? [] as method (method.id)}
					{#if method.kind === 'oidc'}
						<Button class="w-full" onclick={() => void login(method.id)} disabled={waiting}
							>Continue with {method.name}</Button
						>
					{:else}
						<p class="text-sm text-muted-foreground">
							{method.name} uses your gateway sign-in. If access is denied, contact your administrator,
							then retry.
						</p>
					{/if}
				{/each}
				{#if session?.bearer_enabled && !authentication.credential}
					<form class="space-y-3" onsubmit={(event) => void bearerSignIn(event)}>
						<label for="browser-access-key" class="text-sm">Access key</label>
						<Input
							id="browser-access-key"
							type="password"
							bind:value={accessKey}
							autocomplete="off"
							autocapitalize="none"
							spellcheck="false"
							maxlength={128}
							required
						/>
						<Button
							type="submit"
							variant="outline"
							class="w-full"
							disabled={waiting || !accessKey.trim()}>Sign in with access key</Button
						>
					</form>
				{/if}
				{#if !session && !error && !authentication.error && !accessState.message}
					<p class="text-sm text-muted-foreground">
						Refresh the available sign-in methods to continue.
					</p>
				{/if}
				<Button variant="outline" disabled={waiting} onclick={() => void retry()}
					>Retry sign-in</Button
				>
			{/if}
			{#if error || authentication.error || accessState.message}
				<p class="text-sm text-destructive" role="alert">
					{error ?? authentication.error ?? accessState.message}
				</p>
			{/if}
		</main>
	</div>
{/if}
