<script lang="ts">
	import type { PrivacyStatus } from '$lib/types';
	import ShieldCheckIcon from '@lucide/svelte/icons/shield-check';
	import ShieldAlertIcon from '@lucide/svelte/icons/shield-alert';

	type Props = {
		status: PrivacyStatus | null | undefined;
	};

	let { status }: Props = $props();
	let nextTransition = $derived(
		status?.next_transition_at_ms ? new Date(status.next_transition_at_ms).toLocaleString() : null
	);
	let sourceLabel = $derived(
		status?.effective_source === 'default'
			? 'shared default'
			: status?.effective_source === 'override'
				? 'temporary override'
				: status?.effective_source === 'camera'
					? 'camera schedule'
					: 'not configured'
	);
</script>

{#if status?.configured}
	<section
		class="rounded-md border {status.active
			? 'border-amber-500/40 bg-amber-500/10'
			: 'border-hairline bg-surface'} p-4"
		aria-labelledby="privacy-status-heading"
	>
		<div class="flex items-start gap-3">
			{#if status.active}
				<ShieldAlertIcon class="mt-0.5 size-5 shrink-0 text-amber-700" aria-hidden="true" />
			{:else}
				<ShieldCheckIcon class="mt-0.5 size-5 shrink-0 text-emerald-700" aria-hidden="true" />
			{/if}
			<div class="min-w-0 flex-1">
				<div class="flex flex-wrap items-baseline justify-between gap-2">
					<h2 id="privacy-status-heading" class="text-sm font-semibold">
						{status.active ? 'Privacy is enforced now' : 'Privacy schedule is clear'}
					</h2>
					<span class="text-xs font-medium text-muted-foreground">{sourceLabel}</span>
				</div>
				<p class="mt-1 text-xs leading-5 text-muted-foreground">
					{status.active
						? 'Live media, recording, snapshots, event attachments, external services, publication, PTZ, and talkback are blocked by the server.'
						: 'The server will enforce the next scheduled transition for every media path.'}
				</p>
				<dl class="mt-3 grid gap-2 text-xs lg:grid-cols-4 sm:grid-cols-2">
					<div>
						<dt class="text-muted-foreground">Timezone</dt>
						<dd class="font-mono">{status.timezone}</dd>
					</div>
					<div>
						<dt class="text-muted-foreground">Next transition</dt>
						<dd>{nextTransition ?? 'none scheduled'}</dd>
					</div>
					<div>
						<dt class="text-muted-foreground">Revision</dt>
						<dd class="font-mono">{status.revision}</dd>
					</div>
					{#if status.override_reason}
						<div>
							<dt class="text-muted-foreground">Override reason</dt>
							<dd>{status.override_reason}</dd>
						</div>
					{/if}
					{#if status.override_actor}
						<div>
							<dt class="text-muted-foreground">Override actor</dt>
							<dd class="font-mono">{status.override_actor}</dd>
						</div>
					{/if}
				</dl>
				{#if status.error}
					<p class="mt-3 text-xs text-destructive" role="alert">{status.error}</p>
				{/if}
			</div>
		</div>
	</section>
{/if}
