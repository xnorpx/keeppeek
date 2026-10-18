<script lang="ts">
	import { Button } from '$lib/components/ui/button/index.js';
	let {
		archive,
		pending,
		error,
		canRetry = true,
		onretry,
		onclose
	}: {
		archive: boolean;
		pending: boolean;
		error: string | null;
		canRetry?: boolean;
		onretry: () => void;
		onclose: () => void;
	} = $props();
</script>

{#if error || pending}
	<section
		class="space-y-3 rounded-sm border border-hairline bg-surface p-4"
		aria-label={archive ? 'Archive playback' : 'Playback'}
	>
		<div role={error ? 'alert' : 'status'} class="space-y-2">
			<h3 class="text-base font-semibold">
				{error
					? archive
						? 'Archive playback unavailable'
						: 'Playback unavailable'
					: archive
						? 'Preparing archive playback…'
						: 'Preparing playback…'}
			</h3>
			<p class="text-sm text-text-muted">
				{error
					? archive
						? 'Check the archive disk and retry playback. Timeline entries remain available while the destination is offline.'
						: 'Check the displayed error and storage destination, then retry playback. Timeline entries remain available.'
					: 'Reading and verifying the requested footage. Keep your selected camera and position while it loads.'}
			</p>
			{#if error}<p class="text-sm break-words text-text-muted">{error}</p>{/if}
		</div>
		<div class="flex flex-wrap gap-3">
			{#if error}<Button
					type="button"
					variant="outline"
					class="h-11"
					disabled={!canRetry}
					onclick={onretry}>Retry playback</Button
				>{/if}
			<Button type="button" variant="outline" class="h-11" onclick={onclose}>Close playback</Button>
		</div>
	</section>
{:else if archive}
	<div
		role="status"
		aria-label="Archive playback"
		class="flex flex-col gap-1 rounded-sm border border-hairline bg-raised px-4 py-3 sm:flex-row sm:items-center sm:gap-3"
	>
		<span class="font-mono text-xs tracking-wider text-primary-soft">ARCHIVE</span>
		<p class="text-sm text-text-muted">
			Playing from archive. Startup and seeking may take longer.
		</p>
	</div>
{/if}
