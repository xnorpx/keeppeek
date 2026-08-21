<script lang="ts">
	import { resolve } from '$app/paths';
	import type { RankedHealthFinding } from '$lib/health-presentation';

	type Props = {
		finding: RankedHealthFinding | null;
		status: 'healthy' | 'degraded';
		paperFrame?: boolean;
	};

	let { finding, status, paperFrame = false }: Props = $props();
</script>

<section
	class="flex w-full flex-col justify-between gap-4 rounded-md border border-live/40 bg-live/5 px-6 py-[22px] sm:flex-row sm:items-center {paperFrame
		? 'h-[130px] shrink-0'
		: ''}"
	aria-label="Highest priority health issue"
	data-health-priority
>
	<div class="min-w-0 {paperFrame ? 'w-[820px] shrink-0' : ''}">
		<p class="font-mono text-2xs leading-[14px] tracking-caps text-live-text">
			{finding?.issue.severity ?? status} · {finding?.camera ? 'CAMERA RECORDING' : 'SERVER HEALTH'}
		</p>
		<h2 class="mt-1 {paperFrame ? 'text-[28px] leading-[34px]' : 'text-lg'} font-semibold">
			{finding
				? `${finding.camera ? `${finding.camera.name} · ` : ''}${finding.issue.message}`
				: 'No open health findings'}
		</h2>
		<p class="mt-1 text-xs leading-5 text-text-muted {paperFrame ? 'text-sm leading-[22px]' : ''}">
			{finding?.camera?.last_error ??
				(finding
					? `Server-authored health evidence · ${finding.issue.scope}`
					: 'Every reported server and camera check is healthy.')}
		</p>
	</div>
	{#if finding?.camera}
		<a
			href={`${resolve('/system-health')}/camera/${encodeURIComponent(finding.camera.id)}`}
			class="inline-flex shrink-0 items-center justify-center rounded-sm bg-primary font-semibold text-on-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {paperFrame
				? 'h-[34px] px-4 text-[13px]'
				: 'h-9 px-4 text-xs'}"
		>
			Diagnose {finding.camera.name}
		</a>
	{/if}
</section>
