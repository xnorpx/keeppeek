<script lang="ts">
	import '../app.css';
	import { beforeNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import type { Snippet } from 'svelte';

	let { children }: { children: Snippet } = $props();
	const guestDocument = typeof window !== 'undefined' && window.location.pathname === '/share';
	let guestRoute = $derived(page.url.pathname === '/share');
	let documentMatches = $derived(guestRoute === guestDocument);

	beforeNavigate((navigation) => {
		if (!navigation.from || !navigation.to || navigation.willUnload) return;
		if (navigation.from.url.pathname !== '/share' && navigation.to.url.pathname !== '/share')
			return;
		navigation.cancel();
		window.location.assign(navigation.to.url.href);
	});

	$effect.pre(() => {
		if (documentMatches) return;
		// Kit has already changed the URL; reload must create the other security context.
		window.location.reload();
	});
</script>

{#if !documentMatches}
	<p role="status" class="p-6 text-sm text-muted-foreground">Opening the requested page...</p>
{:else if guestDocument}
	{@render children()}
{:else}
	{#await import('$lib/components/AppShell.svelte')}
		<p role="status" class="p-6 text-sm text-muted-foreground">Opening KeepPeek...</p>
	{:then module}
		<module.default {children} />
	{:catch}
		<main class="space-y-4 p-6 text-foreground">
			<p role="alert">KeepPeek could not open. Reload the page to try again.</p>
			<button
				type="button"
				class="min-h-11 rounded-sm border border-border px-4 text-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				onclick={() => window.location.reload()}>Reload page</button
			>
		</main>
	{/await}
{/if}
