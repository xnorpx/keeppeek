<script lang="ts">
	import { resolve } from '$app/paths';
	import type { RankedHealthFinding } from '$lib/health-presentation';

	type Props = {
		finding: RankedHealthFinding | null;
		status: 'healthy' | 'degraded';
		paperFrame?: boolean;
	};

	let { finding, status, paperFrame = false }: Props = $props();
	let tone = $derived(finding?.issue.severity ?? status);
	let surfaceClasses = $derived(
		tone === 'critical'
			? 'border-red-500/40 bg-red-500/8'
			: tone === 'warning' || tone === 'degraded'
				? 'border-amber-500/35 bg-amber-500/8'
				: tone === 'info'
					? 'border-sky-500/30 bg-sky-500/7'
					: 'border-healthy/40 bg-healthy/5'
	);
	let labelClasses = $derived(
		tone === 'critical'
			? 'text-red-700 dark:text-red-300'
			: tone === 'warning' || tone === 'degraded'
				? 'text-amber-800 dark:text-amber-300'
				: tone === 'info'
					? 'text-sky-800 dark:text-sky-300'
					: 'text-healthy'
	);

	function findingHref(): string | null {
		if (!finding?.camera) return null;
		if (finding.issue.timeline_start_ms !== null && finding.issue.timeline_start_ms !== undefined) {
			const date = new Date(finding.issue.timeline_start_ms).toISOString().slice(0, 10);
			const search = new URLSearchParams({
				camera: finding.camera.id,
				date,
				at: String(finding.issue.timeline_start_ms)
			});
			return `${resolve('/keep')}?${search}`;
		}
		return `${resolve('/system-health')}/camera/${encodeURIComponent(finding.camera.id)}`;
	}
</script>

<section
	class="flex w-full flex-col justify-between gap-4 rounded-md border px-6 py-[22px] sm:flex-row sm:items-center {surfaceClasses} {paperFrame
		? 'h-[130px] shrink-0'
		: ''}"
	aria-label="Highest priority health issue"
	data-health-priority
	data-health-priority-severity={tone}
>
	<div class="min-w-0 {paperFrame ? 'w-[820px] shrink-0' : ''}">
		<p class="font-mono text-2xs leading-[14px] tracking-caps {labelClasses}">
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
	{#if finding?.camera && findingHref()}
		<a
			href={findingHref() ?? undefined}
			class="inline-flex shrink-0 items-center justify-center rounded-sm bg-primary font-semibold text-on-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {paperFrame
				? 'h-[34px] px-4 text-[13px]'
				: 'h-9 px-4 text-xs'}"
		>
			{finding.issue.operational_event_id ? 'Open outage' : `Diagnose ${finding.camera.name}`}
		</a>
	{/if}
</section>
