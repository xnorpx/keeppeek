<script lang="ts">
	type Props = {
		answeredCount: number;
		elapsedMs: number;
		durationMs?: number;
		subnetCount?: number;
		probesSent?: number;
		totalProbes?: number;
		progressOverride?: number;
		class?: string;
	};

	let {
		answeredCount,
		elapsedMs,
		durationMs = 5_000,
		subnetCount = 0,
		probesSent,
		totalProbes,
		progressOverride,
		class: className = ''
	}: Props = $props();
	let progress = $derived(Math.max(0, Math.min(1, progressOverride ?? elapsedMs / durationMs)));
	let probeEvidence = $derived(
		probesSent !== undefined && totalProbes !== undefined
			? `${probesSent} of ${totalProbes} probes sent`
			: `${subnetCount} /24 ${subnetCount === 1 ? 'network' : 'networks'} · protocols parallel`
	);
	let elapsedLabel = $derived(
		`${(elapsedMs / 1_000).toFixed(1)}s elapsed · ${durationMs / 1_000}s target`
	);
</script>

<div
	data-discovery-progress
	class="flex flex-col justify-center gap-3 bg-raised p-[18px] {className}"
	role="status"
	aria-label="Camera discovery progress"
>
	<div class="flex items-baseline gap-2.5">
		<span class="text-3xl leading-[42px] font-bold tracking-tight">{answeredCount}</span>
		<span class="text-base leading-5 text-text-muted">
			{answeredCount === 1 ? 'device answered' : 'devices answered'} so far
		</span>
	</div>
	<div
		class="h-1 overflow-hidden rounded-full bg-hairline"
		role="progressbar"
		aria-label="Discovery time target"
		aria-valuemin="0"
		aria-valuemax={durationMs}
		aria-valuenow={Math.min(durationMs, Math.round(elapsedMs))}
		aria-valuetext={elapsedLabel}
	>
		<div class="h-full bg-primary" style:width={`${progress * 100}%`}></div>
	</div>
	<div
		class="flex items-center justify-between font-mono text-2xs leading-3 tracking-caps text-text-faint uppercase"
	>
		<span>{probeEvidence}</span>
		<span>{elapsedLabel}</span>
	</div>
</div>
