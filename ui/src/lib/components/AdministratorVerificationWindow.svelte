<script lang="ts">
	import { onMount } from 'svelte';
	import {
		submitProviderLogin,
		verificationOpenerOrigin,
		verificationStartFromMessage
	} from '$lib/external-authentication.svelte';
	import { Button } from '$lib/components/ui/button/index.js';

	let message = $state('Waiting for the administrator verification request.');
	let failed = $state(false);
	onMount(() => {
		// The callback uses an app query because the server rejects fragments in return paths.
		if (new URL(window.location.href).searchParams.get('verification') === 'complete') {
			message = 'Provider sign-in completed. Return to the original window to finish verification.';
			return;
		}
		const opener: Window | null = window.opener;
		const openerOrigin = verificationOpenerOrigin(new URL(window.location.href));
		if (!opener || !openerOrigin) {
			failed = true;
			message = 'Open verification from the original KeepPeek administration window.';
			return;
		}
		const origin = window.location.origin;
		const receive = (event: MessageEvent) => {
			const start = verificationStartFromMessage(event, opener, origin, openerOrigin);
			if (!start) return;
			cleanup();
			try {
				message =
					'Verification submitted. Complete provider sign-in, then return to the original window to finish verification.';
				submitProviderLogin(
					start.provider_id,
					start.csrf_token,
					'/?verification=complete',
					start.candidate_plan_id
				);
			} catch {
				failed = true;
				message = 'Verification could not start. Return to the original window and try again.';
			}
		};
		const timer = setTimeout(() => {
			cleanup();
			failed = true;
			message = 'This verification request expired. Return to the original window and start again.';
		}, 60_000);
		function cleanup(): void {
			clearTimeout(timer);
			window.removeEventListener('message', receive);
		}
		window.addEventListener('message', receive);
		opener.postMessage({ type: 'keeppeek-verification-ready' }, openerOrigin);
		return cleanup;
	});
</script>

<svelte:head><title>Verify administrator · KeepPeek</title></svelte:head>
<div class="grid min-h-svh place-items-center bg-background px-5 py-10 text-foreground">
	<main class="w-full max-w-sm space-y-5" aria-labelledby="verification-heading">
		<p class="text-sm font-semibold">KeepPeek</p>
		<h1 id="verification-heading" class="text-xl font-semibold">Verify administrator access</h1>
		<p class="text-sm text-muted-foreground" role={failed ? 'alert' : 'status'}>{message}</p>
		<Button variant="outline" onclick={() => window.close()}>Close verification window</Button>
	</main>
</div>
