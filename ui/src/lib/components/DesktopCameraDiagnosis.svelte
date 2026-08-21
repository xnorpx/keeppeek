<script lang="ts">
	import { resolve } from '$app/paths';
	import type { CameraDiagnosisEvidence } from '$lib/health-presentation';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import NetworkIcon from '@lucide/svelte/icons/network';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RouterIcon from '@lucide/svelte/icons/router';
	import CapabilityGate from './CapabilityGate.svelte';

	type Props = {
		evidence: CameraDiagnosisEvidence;
		runtimeConfigSupported?: boolean;
		updatingTransport?: boolean;
		statusMessage?: string | null;
		errorMessage?: string | null;
		onswitchtotcp?: () => void | Promise<void>;
	};

	let {
		evidence,
		runtimeConfigSupported = false,
		updatingTransport = false,
		statusMessage = null,
		errorMessage = null,
		onswitchtotcp
	}: Props = $props();

	let primaryMessage = $derived(
		evidence.relatedIssues[0]?.message ?? evidence.camera.last_error ?? 'No active camera issue'
	);
	let configuredStreams = $derived(
		evidence.camera.configured_profiles.map((profile) => profile.stream).join(' + ') ||
			'profiles unavailable'
	);

	function cameraHref(): string {
		return `${resolve('/camera')}?camera=${encodeURIComponent(evidence.camera.id)}`;
	}

	function deviceHref(): string {
		const { ip } = evidence.camera;
		return `http://${ip.includes(':') ? `[${ip}]` : ip}`;
	}

	function formatCounter(value: number | null): string {
		return value === null ? 'Unavailable' : new Intl.NumberFormat().format(value);
	}

	function cameraCountLabel(value: number): string {
		return `${value} ${value === 1 ? 'camera' : 'cameras'}`;
	}

	function formatObservationTime(value: number | null): string {
		return value === null ? 'No stream report available' : new Date(value).toLocaleString();
	}

	function stateClasses(): string {
		if (evidence.camera.state === 'online') return 'border-healthy/40 bg-healthy/10 text-healthy';
		if (evidence.camera.state === 'degraded' || evidence.camera.state === 'stale') {
			return 'border-activity/50 bg-activity/10 text-activity';
		}
		return 'border-live/40 bg-live/10 text-live-text';
	}
</script>

<div data-desktop-camera-diagnosis>
	<header
		data-camera-diagnosis-context
		class="flex min-h-[52px] flex-wrap items-center gap-3 border-b border-hairline bg-surface px-4 py-2 md:h-[52px] md:px-5 md:py-0"
	>
		<a
			href={resolve('/system-health')}
			class="text-sm leading-4 text-text-muted"
			aria-label="Back to Health"
		>
			Health
		</a>
		<ChevronRightIcon class="size-3 shrink-0 text-text-faint" strokeWidth={1.75} />
		<h1 class="truncate text-xl leading-6 font-semibold">{evidence.camera.name}</h1>
		<span class="rounded-full border px-2.5 py-1 font-mono text-2xs uppercase {stateClasses()}"
			>{evidence.camera.state}</span
		>
		<p
			class="hidden truncate font-mono text-xs leading-[14px] tracking-caps text-text-faint uppercase lg:block"
		>
			{evidence.camera.ip} · {evidence.camera.manufacturer ?? 'Unknown manufacturer'}
			{evidence.camera.model ?? ''} · {configuredStreams}
		</p>
		<div class="ml-auto flex flex-wrap gap-2">
			<a
				href={cameraHref()}
				class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
			>
				<CameraIcon class="size-3.5" /> Open camera page
			</a>
			<button
				type="button"
				class="inline-flex h-8 items-center gap-2 rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary disabled:cursor-not-allowed disabled:opacity-45"
				disabled
				title="The server does not expose a camera retry command"
			>
				<RefreshCwIcon class="size-3.5" /> Retry now
			</button>
		</div>
	</header>

	{#if errorMessage}<p
			class="mx-4 mt-4 rounded-sm border border-live/40 bg-live/5 px-3 py-2 text-xs text-live-text md:mx-6"
			role="alert"
		>
			{errorMessage}
		</p>{/if}
	{#if statusMessage}<p
			class="mx-4 mt-4 rounded-sm border border-healthy/40 bg-healthy/5 px-3 py-2 text-xs text-text-muted md:mx-6"
			role="status"
		>
			{statusMessage}
		</p>{/if}

	<div
		data-camera-diagnosis-body
		class="grid items-start gap-4 p-4 md:p-6 lg:grid-cols-[840px_464px] lg:gap-6"
	>
		<div class="space-y-5">
			<section
				class="overflow-hidden rounded-lg border border-live/40 bg-surface lg:h-[197px]"
				aria-labelledby="diagnosis-heading"
			>
				<header class="min-h-[105px] border-b border-hairline bg-live/5 px-5 py-[18px]">
					<p class="font-mono text-2xs tracking-caps text-live-text">
						SERVER-OBSERVED CAMERA EVIDENCE
					</p>
					<h2 id="diagnosis-heading" class="mt-1 text-lg font-semibold">{primaryMessage}</h2>
					<p class="mt-1 text-xs leading-5 text-text-muted">
						Lifecycle {evidence.camera.lifecycle ?? 'unavailable'} · Last error {evidence.camera
							.last_error ?? 'unavailable'}
					</p>
				</header>
				<dl class="grid sm:h-[90px] sm:grid-cols-3">
					<div class="border-b border-hairline px-4 py-3 sm:border-r sm:border-b-0">
						<dt class="font-mono text-2xs tracking-caps text-text-faint">LATEST STREAM REPORT</dt>
						<dd class="mt-1 text-xs text-text-muted">
							{formatObservationTime(evidence.latestStreamReportAtMs)}
						</dd>
					</div>
					<div class="border-b border-hairline px-4 py-3 sm:border-r sm:border-b-0">
						<dt class="font-mono text-2xs tracking-caps text-text-faint">RECONNECTS OBSERVED</dt>
						<dd class="mt-1 text-sm font-medium">{formatCounter(evidence.reconnects)}</dd>
					</div>
					<div class="px-4 py-3">
						<dt class="font-mono text-2xs tracking-caps text-text-faint">NEXT RETRY</dt>
						<dd class="mt-1 text-sm font-medium">Unavailable in health API</dd>
					</div>
				</dl>
			</section>

			<section
				class="overflow-hidden rounded-lg border border-hairline bg-surface lg:h-[457px]"
				aria-labelledby="steps-heading"
			>
				<header
					class="flex h-[57px] flex-wrap items-center justify-between gap-2 border-b border-hairline px-5"
				>
					<h2 id="steps-heading" class="text-lg font-semibold">Try in this order</h2>
					<span class="font-mono text-2xs tracking-caps text-text-faint"
						>CHEAPEST EVIDENCE FIRST</span
					>
				</header>
				<ol class="divide-y divide-hairline">
					<li
						class="grid min-h-[95px] gap-3 px-5 py-4 sm:grid-cols-[24px_minmax(0,1fr)_auto] sm:items-start"
					>
						<span
							class="grid size-6 place-items-center rounded-full border border-hairline-strong bg-raised font-mono text-2xs"
							>1</span
						>
						<div>
							<h3 class="text-lg leading-5 font-semibold">Open the camera on this network</h3>
							<p class="mt-1 text-sm leading-[19px] text-text-muted">
								Confirms power and address before changing KeepPeek configuration.
							</p>
						</div>
						<a
							href={deviceHref()}
							target="_blank"
							rel="noreferrer"
							class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
						>
							<ExternalLinkIcon class="size-3.5" /> Open {evidence.camera.ip}
						</a>
					</li>
					<li
						class="grid min-h-[95px] gap-3 px-5 py-4 sm:grid-cols-[24px_minmax(0,1fr)_auto] sm:items-start"
					>
						<span
							class="grid size-6 place-items-center rounded-full border border-hairline-strong bg-raised font-mono text-2xs"
							>2</span
						>
						<div>
							<h3 class="text-lg leading-5 font-semibold">Test the configured login</h3>
							<p class="mt-1 text-sm leading-[19px] text-text-muted">
								Credentials are write-only and no candidate authentication probe endpoint exists.
							</p>
						</div>
						<button
							type="button"
							class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline bg-raised px-3 text-xs text-text-muted disabled:cursor-not-allowed"
							disabled
						>
							<KeyRoundIcon class="size-3.5" /> Probe unavailable
						</button>
					</li>
					<li
						class="grid min-h-[114px] gap-3 bg-activity/5 px-5 py-4 sm:grid-cols-[24px_minmax(0,1fr)_auto] sm:items-start"
					>
						<span
							class="grid size-6 place-items-center rounded-full border border-hairline-strong bg-raised font-mono text-2xs"
							>3</span
						>
						<div>
							<h3 class="text-lg leading-5 font-semibold">
								{evidence.canSuggestTcp
									? 'Switch this camera to TCP'
									: 'Review transport and ports'}
							</h3>
							<p class="mt-1 text-sm leading-[19px] text-text-muted">
								{evidence.canSuggestTcp
									? 'The current camera setting is UDP. This action changes transport only.'
									: `The current camera setting is ${evidence.camera.transport ?? 'unavailable'}; health does not justify a transport change.`}
							</p>
						</div>
						{#if evidence.canSuggestTcp}
							{#if runtimeConfigSupported}
								<button
									type="button"
									class="inline-flex h-8 items-center gap-2 rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary disabled:opacity-45"
									disabled={updatingTransport || onswitchtotcp === undefined}
									onclick={() => void onswitchtotcp?.()}
								>
									<NetworkIcon class="size-3.5" />
									{updatingTransport ? 'Saving' : 'Switch to TCP'}
								</button>
							{:else}
								<CapabilityGate action="Switch to TCP" capability="keeppeek.runtime-config.v1" />
							{/if}
						{:else}
							<a
								href={cameraHref()}
								class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
								><RouterIcon class="size-3.5" /> Review settings</a
							>
						{/if}
					</li>
					<li
						class="grid min-h-[94px] gap-3 px-5 py-4 sm:grid-cols-[24px_minmax(0,1fr)_auto] sm:items-start"
					>
						<span
							class="grid size-6 place-items-center rounded-full border border-hairline-strong bg-raised font-mono text-2xs"
							>4</span
						>
						<div>
							<h3 class="text-lg leading-5 font-semibold">Inspect redacted logs</h3>
							<p class="mt-1 text-sm leading-[19px] text-text-muted">
								Use browser/server logs only after the direct device checks above.
							</p>
						</div>
						<a
							href={resolve('/settings/logs')}
							class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
							><FileTextIcon class="size-3.5" /> Open logs</a
						>
					</li>
				</ol>
			</section>
		</div>

		<aside class="space-y-5" aria-label="Diagnosis evidence summary">
			<section
				class="h-[279px] rounded-lg border border-hairline bg-surface p-[18px]"
				aria-labelledby="stream-evidence-heading"
			>
				<h2 id="stream-evidence-heading" class="flex items-center gap-2 text-base font-semibold">
					<NetworkIcon class="size-4" /> Stream evidence
				</h2>
				<dl class="mt-3 divide-y divide-hairline border-y border-hairline text-xs">
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Drops</dt>
						<dd class="font-mono">{formatCounter(evidence.drops)}</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Errors</dt>
						<dd class="font-mono">{formatCounter(evidence.errors)}</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Configured profiles</dt>
						<dd class="font-mono">{evidence.camera.configured_profiles.length}</dd>
					</div>
				</dl>
			</section>

			<section
				class="h-[185px] rounded-lg border border-hairline bg-surface p-[18px]"
				aria-labelledby="scope-heading"
			>
				<h2 id="scope-heading" class="flex items-center gap-2 text-base font-semibold">
					<RouterIcon class="size-4" /> Is anything else affected?
				</h2>
				<div class="mt-3 space-y-2 text-xs text-text-muted">
					<p class="flex items-center gap-2">
						<span class="size-1.5 rounded-full bg-healthy"></span>{cameraCountLabel(
							evidence.reportingNormally
						)} currently {evidence.reportingNormally === 1 ? 'reports' : 'report'} online
					</p>
					<p class="flex items-center gap-2">
						<span class="size-1.5 rounded-full bg-activity"></span>{cameraCountLabel(
							evidence.otherUnhealthyCameras
						)} other than this one {evidence.otherUnhealthyCameras === 1 ? 'is' : 'are'} degraded, stale,
						or offline
					</p>
				</div>
			</section>

			<section
				class="h-[130px] rounded-lg border border-live/40 bg-live/5 p-[18px]"
				aria-labelledby="impact-heading"
			>
				<p class="font-mono text-2xs tracking-caps text-live-text">RECORDING IMPACT</p>
				<h2 id="impact-heading" class="mt-1 text-base font-semibold">Gap start unavailable</h2>
				<p class="mt-1 text-xs leading-5 text-text-muted">
					ServerHealthResponse does not include the last accepted recording timestamp. Keep can draw
					known catalog gaps, but this diagnosis cannot claim a missing-footage duration.
				</p>
			</section>
		</aside>
	</div>
</div>
