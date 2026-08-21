<script lang="ts">
	import { resolve } from '$app/paths';
	import { useCapabilityState } from '$lib/capability-context';
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import type { CameraDiagnosisEvidence } from '$lib/health-presentation';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';

	type Props = {
		evidence: CameraDiagnosisEvidence;
		generatedAtMs: number;
		updatingTransport?: boolean;
		statusMessage?: string | null;
		errorMessage?: string | null;
		actionFixed?: boolean;
		onswitchtotcp?: () => void | Promise<void>;
	};

	let {
		evidence,
		generatedAtMs,
		updatingTransport = false,
		statusMessage = null,
		errorMessage = null,
		actionFixed = true,
		onswitchtotcp
	}: Props = $props();
	const capabilities = useCapabilityState();
	let streamMode = $derived(evidence.camera.state === 'degraded');
	let frameDropPercent = $derived.by(() => {
		const frames = evidence.camera.streams.reduce(
			(total, stream) => total + (stream.frames ?? 0),
			0
		);
		const drops = evidence.drops ?? 0;
		return frames + drops > 0 ? Math.round((drops / (frames + drops)) * 100) : null;
	});

	function formatAge(timestampMs: number | null): string {
		if (timestampMs === null) return 'Unavailable';
		const ageSeconds = Math.max(0, Math.round((generatedAtMs - timestampMs) / 1_000));
		const hours = Math.floor(ageSeconds / 3_600);
		const minutes = Math.floor((ageSeconds % 3_600) / 60);
		if (hours > 0) return `${hours}h ${minutes}m ago`;
		if (minutes > 0) return `${minutes}m ago`;
		return `${ageSeconds}s ago`;
	}

	function formatCount(value: number | null): string {
		return value === null ? 'Unavailable' : new Intl.NumberFormat().format(value);
	}

	function primaryMessage(): string {
		return (
			evidence.camera.last_error ?? evidence.relatedIssues[0]?.message ?? 'No active camera issue'
		);
	}
</script>

<section
	data-mobile-camera-diagnosis={streamMode ? 'stream' : 'issue'}
	class="flex w-full flex-col md:hidden"
	aria-label={streamMode ? 'Mobile stream evidence' : 'Mobile camera issue'}
>
	<header class="flex h-[52px] shrink-0 items-center gap-3 border-b border-hairline px-4">
		<a
			href={resolve('/system-health')}
			class="grid size-[18px] shrink-0 place-items-center focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			aria-label="Back to Health"
		>
			<ChevronLeftIcon class="size-[18px]" strokeWidth={2} />
		</a>
		<h1 class="truncate text-lg leading-5 font-semibold">
			{evidence.camera.name} · {streamMode ? 'Stream evidence' : 'Offline'}
		</h1>
	</header>

	{#if streamMode}
		<div class="flex h-[660px] shrink-0 flex-col gap-[14px] p-4">
			<div
				class="flex h-[42px] shrink-0 items-center gap-[9px] rounded-sm border border-activity/35 bg-activity/10 px-3"
			>
				<span class="size-[7px] shrink-0 rounded-full bg-activity"></span>
				<p class="truncate text-sm leading-4">
					{errorMessage ??
						statusMessage ??
						`Degraded · ${frameDropPercent ?? 'unknown'}% frames dropped`}
				</p>
			</div>

			<div
				class="flex h-[63px] shrink-0 gap-px overflow-hidden rounded-sm border border-hairline bg-hairline"
			>
				<div class="flex w-[119px] shrink-0 flex-col gap-[3px] bg-surface p-3">
					<p class="font-mono text-2xs leading-3 text-text-faint">DROPS OBSERVED</p>
					<p class="text-lg-plus leading-[22px] text-activity">{formatCount(evidence.drops)}</p>
				</div>
				<div class="flex w-[119px] shrink-0 flex-col gap-[3px] bg-surface p-3">
					<p class="font-mono text-2xs leading-3 text-text-faint">RECONNECTS</p>
					<p class="text-lg-plus leading-[22px]">{formatCount(evidence.reconnects)}</p>
				</div>
				<div class="flex w-[118px] shrink-0 flex-col gap-[3px] bg-surface p-3">
					<p class="font-mono text-2xs leading-3 text-text-faint">REPORT AGE</p>
					<p class="text-lg-plus leading-[22px]">{formatAge(evidence.latestStreamReportAtMs)}</p>
				</div>
			</div>

			<h2 class="h-5 shrink-0 text-lg leading-5 font-semibold">Last 30 minutes</h2>
			<div
				class="grid h-[116px] shrink-0 place-items-center rounded-md border border-hairline bg-surface text-center"
			>
				<div>
					<p class="text-sm leading-4 font-medium">History unavailable</p>
					<p class="mt-1 font-mono text-2xs leading-3 text-text-faint">CURRENT COUNTERS ONLY</p>
				</div>
			</div>

			<div
				class="flex h-[129px] shrink-0 flex-col gap-2 rounded-md border border-hairline bg-surface p-[14px]"
			>
				<div class="flex items-center justify-between">
					<h2 class="text-md leading-[18px] font-semibold">Current evidence</h2>
					<span class="font-mono text-2xs leading-3 text-activity">NO CAUSAL CONFIDENCE</span>
				</div>
				<p class="text-sm leading-[19.5px] text-text-muted">
					{formatCount(evidence.drops)} drops observed on {evidence.camera.transport?.toUpperCase() ??
						'an unknown'} transport. Health does not identify a cause.
				</p>
				<div class="mt-auto flex items-center justify-between border-t border-hairline pt-2">
					<span class="text-sm leading-4">Switch to TCP</span>
					<span class="font-mono text-2xs leading-3 text-healthy">
						{capabilities.supports('keeppeek.runtime-config.v1')
							? 'SHIPS · CAMERA WRITE'
							: 'SERVER UPDATE REQUIRED'}
					</span>
				</div>
			</div>

			<p class="text-xs-plus leading-[18px] text-text-faint">
				The action changes transport only. It never rewrites unrelated camera settings.
			</p>
		</div>
	{:else}
		<div class="flex h-[660px] shrink-0 flex-col gap-[14px] p-4">
			<div class="flex h-[88px] shrink-0 flex-col gap-1.5">
				<p class="font-mono text-2xs leading-3 tracking-[0.08em] text-live-text">
					RECORDING GAP START UNAVAILABLE
				</p>
				<h2 class="text-xl leading-6 font-semibold">{primaryMessage()}</h2>
				<p class="line-clamp-2 text-sm leading-[19.5px] text-text-muted">
					Current health reports the camera error, but not a recording-gap start or retry schedule.
				</p>
			</div>

			<dl class="h-[182px] shrink-0 rounded-md border border-hairline bg-surface p-[14px]">
				<div class="flex h-[38px] items-center justify-between border-b border-hairline">
					<dt class="text-sm leading-4 text-text-muted">Address</dt>
					<dd class="font-mono text-xs-plus leading-4">{evidence.camera.ip}</dd>
				</div>
				<div class="flex h-[38px] items-center justify-between border-b border-hairline">
					<dt class="text-sm leading-4 text-text-muted">Latest stream report</dt>
					<dd class="font-mono text-xs-plus leading-4 text-live-text">
						{formatAge(evidence.latestStreamReportAtMs)}
					</dd>
				</div>
				<div class="flex h-[38px] items-center justify-between border-b border-hairline">
					<dt class="text-sm leading-4 text-text-muted">Reconnects observed</dt>
					<dd class="font-mono text-xs-plus leading-4">{formatCount(evidence.reconnects)}</dd>
				</div>
				<div class="flex h-[38px] items-center justify-between">
					<dt class="text-sm leading-4 text-text-muted">Next retry</dt>
					<dd class="font-mono text-xs-plus leading-4">Unavailable</dd>
				</div>
			</dl>

			<h2 class="h-5 shrink-0 text-lg leading-5 font-semibold">Try in this order</h2>
			<ol class="contents">
				<li class="flex h-[74px] shrink-0 gap-3 rounded-sm bg-surface p-3">
					<span
						class="grid size-6 shrink-0 place-items-center rounded-full bg-primary font-mono text-xs leading-[14px] text-on-primary"
						>1</span
					>
					<div class="flex flex-col gap-0.5">
						<h3 class="text-sm leading-4 font-semibold">Open the camera on the LAN</h3>
						<p class="text-xs-plus leading-4 text-text-muted">
							Confirms power and address before changing configuration.
						</p>
					</div>
				</li>
				<li class="flex h-[74px] shrink-0 gap-3 rounded-sm bg-surface p-3">
					<span
						class="grid size-6 shrink-0 place-items-center rounded-full bg-primary font-mono text-xs leading-[14px] text-on-primary"
						>2</span
					>
					<div class="flex flex-col gap-0.5">
						<h3 class="text-sm leading-4 font-semibold">Test the configured login</h3>
						<p class="text-xs-plus leading-4 text-text-muted">
							Credential probe unavailable; secrets remain write-only.
						</p>
					</div>
				</li>
				<li class="flex h-[58px] shrink-0 gap-3 rounded-sm bg-surface p-3">
					<span
						class="grid size-6 shrink-0 place-items-center rounded-full bg-primary font-mono text-xs leading-[14px] text-on-primary"
						>3</span
					>
					<div class="flex flex-col gap-0.5">
						<h3 class="text-sm leading-4 font-semibold">Review transport and ports</h3>
						<p class="text-xs-plus leading-4 text-text-muted">
							Change only after direct device evidence.
						</p>
					</div>
				</li>
			</ol>
		</div>
	{/if}

	<footer
		data-mobile-diagnosis-action
		class="{actionFixed
			? 'fixed inset-x-0 bottom-0 z-50'
			: 'relative'} flex h-[68px] shrink-0 items-start justify-center border-t border-hairline bg-surface px-4 pt-2.5 pb-5 md:hidden"
	>
		{#if streamMode && evidence.canSuggestTcp}
			{#if capabilities.supports('keeppeek.runtime-config.v1')}
				<button
					type="button"
					class="h-[38px] w-full rounded-sm bg-primary text-md leading-[18px] font-semibold text-on-primary disabled:opacity-45"
					disabled={updatingTransport}
					onclick={() => void onswitchtotcp?.()}
				>
					{updatingTransport ? 'Saving transport' : 'Switch to TCP and test'}
				</button>
			{:else}
				<CapabilityGate
					action="Switch to TCP"
					capability="keeppeek.runtime-config.v1"
					class="h-[38px] min-h-0 w-full justify-center text-sm"
				/>
			{/if}
		{:else}
			<button
				type="button"
				class="h-[38px] w-full rounded-sm border border-hairline bg-raised text-md leading-[18px] font-semibold text-text-muted"
				disabled
				title="The server does not expose a camera retry command"
			>
				Retry unavailable
			</button>
		{/if}
	</footer>
</section>
