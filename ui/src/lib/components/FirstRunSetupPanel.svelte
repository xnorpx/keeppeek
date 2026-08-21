<script lang="ts">
	import { resolve } from '$app/paths';
	import { capabilityActions } from '$lib/capability-actions';
	import { firstRunStorageEvidence } from '$lib/first-run';
	import type { DiskHealth, SanitizedConfig } from '$lib/types';
	import ArrowRightIcon from '@lucide/svelte/icons/arrow-right';
	import ClockIcon from '@lucide/svelte/icons/clock-3';
	import HardDriveIcon from '@lucide/svelte/icons/hard-drive';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import CapabilityGate from './CapabilityGate.svelte';

	type Props = {
		config: SanitizedConfig;
		health: { version: string; system: { disks: readonly DiskHealth[] } } | null;
		timeZone: string | null;
		paperFrame?: boolean;
	};

	let { config, health, timeZone, paperFrame = false }: Props = $props();
	let storageEvidence = $derived(
		firstRunStorageEvidence(config.storage.medium_term_path, health?.system.disks ?? [], null)
	);
	let storageCapacityLabel = $derived.by(() => {
		if (storageEvidence.availableBytes === null) return 'CAPACITY UNAVAILABLE';
		return `${formatBytes(storageEvidence.availableBytes)} FREE · CAPACITY OBSERVED`;
	});

	const byteUnits = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] as const;

	function formatBytes(bytes: number): string {
		if (bytes <= 0) return '0 B';
		const unitIndex = Math.min(Math.floor(Math.log(bytes) / Math.log(1000)), byteUnits.length - 1);
		const value = bytes / 1000 ** unitIndex;
		return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value)} ${byteUnits[unitIndex]}`;
	}
</script>

<section
	data-first-run-panel
	class="flex flex-col overflow-hidden rounded-md border border-hairline-strong bg-surface {paperFrame
		? 'h-[785px] w-[708px] shrink-0'
		: 'w-full'}"
	aria-labelledby="setup-heading"
>
	<header
		class="flex flex-col border-b border-hairline {paperFrame
			? 'h-[189px] shrink-0 gap-3 px-8 pt-8 pb-6'
			: 'gap-2 px-5 py-4 md:px-6'}"
	>
		<div class="flex items-center gap-3">
			<span
				class="grid place-items-center rounded-sm bg-primary font-semibold text-on-primary {paperFrame
					? 'size-[34px] text-base leading-5'
					: 'size-9'}">K</span
			>
			<h2
				id="setup-heading"
				class="font-semibold {paperFrame ? 'text-[28px] leading-[34px]' : 'text-xl'}"
			>
				KeepPeek
			</h2>
		</div>
		<p
			class="text-text-muted {paperFrame
				? 'h-12 w-[600px] text-[15px] leading-[23px]'
				: 'max-w-2xl text-sm leading-6'}"
		>
			Nothing has left this machine. This host is administrator over loopback. Other devices need
			the configured remote key; storage and time are not optional.
		</p>
		{#if health}
			<span
				class="inline-flex h-[26px] items-center gap-2 self-start rounded-full bg-activity/10 px-2.5 font-mono text-2xs leading-[14px] tracking-[0.08em] {paperFrame
					? 'w-[251px]'
					: ''}"
			>
				<span class="size-1.5 rounded-full bg-activity"></span>
				PROOF OF CONCEPT · {health.version}
			</span>
		{/if}
	</header>

	<ol
		class="flex flex-col {paperFrame
			? 'h-[515px] shrink-0 gap-[22px] px-8 py-[26px]'
			: 'gap-4 px-5 py-4 md:px-6'}"
	>
		<li class="grid grid-cols-[24px_minmax(0,1fr)] {paperFrame ? 'h-[243px] gap-4' : 'gap-3'}">
			<span
				class="grid size-6 place-items-center rounded-full bg-primary font-mono text-2xs font-semibold text-on-primary"
				>1</span
			>
			<div
				class="min-w-0 {paperFrame
					? 'flex h-[243px] w-[604px] shrink-0 flex-col gap-2'
					: 'space-y-2'}"
			>
				<div class="flex h-5 items-center gap-2">
					{#if !paperFrame}<HardDriveIcon class="size-4 text-text-muted" />{/if}
					<h3 class="text-sm font-semibold {paperFrame ? 'text-base leading-5' : ''}">
						Where footage goes
					</h3>
				</div>
				<div
					class="flex flex-wrap items-center justify-between gap-2 rounded-sm border border-primary bg-raised px-3 {paperFrame
						? 'h-[38px] shrink-0'
						: 'min-h-10'}"
				>
					<code
						class="break-all text-foreground {paperFrame ? 'text-sm leading-[18px]' : 'text-xs'}"
						>{storageEvidence.path}</code
					>
					<span class="font-mono text-2xs text-healthy">{storageCapacityLabel}</span>
				</div>
				{#if paperFrame}
					<p class="h-9 text-xs-plus leading-[18px] text-text-faint">
						Capacity is observed from the recording disk. The server does not expose a candidate
						write probe.
					</p>
				{:else if storageEvidence.diskName}
					<p class="font-mono text-2xs text-text-faint">
						{storageEvidence.diskName} · mounted at {storageEvidence.mountPoint}
					</p>
				{/if}
				<div
					class="rounded-sm border border-live/40 bg-live/5 {paperFrame
						? 'flex h-[125px] shrink-0 flex-col gap-[7px] px-3.5 py-3'
						: 'px-3 py-2.5'}"
					role="status"
					data-storage-write-status={storageEvidence.writeStatus}
				>
					<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-live-text">
						<TriangleAlertIcon class="size-3.5 shrink-0" /> WRITE PROOF UNAVAILABLE
					</div>
					<p class="text-xs leading-[18px] text-text-muted">{storageEvidence.detail}</p>
					{#if paperFrame}
						<p class="font-mono text-xs leading-4 text-text-faint">SERVER WRITE PROBE REQUIRED</p>
					{/if}
				</div>
				{#if !paperFrame}
					<a
						href={resolve('/settings')}
						class="inline-flex h-7 items-center rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
						>Review storage settings</a
					>
				{/if}
			</div>
		</li>

		<li class="grid grid-cols-[24px_minmax(0,1fr)] {paperFrame ? 'h-[66px] gap-4' : 'gap-3'}">
			<span
				class="grid size-6 place-items-center rounded-full bg-primary font-mono text-2xs font-semibold text-on-primary"
				>2</span
			>
			<div
				class="min-w-0 {paperFrame
					? 'flex h-[66px] w-[604px] shrink-0 flex-col gap-2'
					: 'space-y-2'}"
			>
				<div class="flex h-5 items-center gap-2">
					{#if !paperFrame}<ClockIcon class="size-4 text-text-muted" />{/if}
					<h3 class="text-sm font-semibold {paperFrame ? 'text-base leading-5' : ''}">
						What time it is here
					</h3>
				</div>
				<div
					class="flex flex-wrap items-center justify-between gap-2 rounded-sm border border-hairline-strong bg-raised px-3 {paperFrame
						? 'h-[38px] shrink-0'
						: 'min-h-10'}"
				>
					<span class="text-sm">{timeZone ?? 'Unavailable'}</span>
					<span class="font-mono text-2xs text-text-faint">DETECTED FROM THIS BROWSER</span>
				</div>
				{#if !paperFrame}
					<p class="text-xs leading-5 text-text-faint">
						The server does not expose its timezone or a timezone update command, so this is not
						claimed as machine evidence.
					</p>
				{/if}
			</div>
		</li>

		<li class="grid grid-cols-[24px_minmax(0,1fr)] {paperFrame ? 'h-[110px] gap-4' : 'gap-3'}">
			<span
				class="grid size-6 place-items-center rounded-full bg-primary font-mono text-2xs font-semibold text-on-primary"
				>3</span
			>
			<div
				class="min-w-0 {paperFrame
					? 'flex h-[110px] w-[604px] shrink-0 flex-col gap-2'
					: 'space-y-2'}"
			>
				<div class="flex h-5 items-center gap-2">
					{#if !paperFrame}<KeyRoundIcon class="size-4 text-text-muted" />{/if}
					<h3 class="text-sm font-semibold {paperFrame ? 'text-base leading-5' : ''}">
						Remote sign-in (optional{paperFrame ? ' — skip it' : ''})
					</h3>
				</div>
				<CapabilityGate
					{...capabilityActions.remoteSignIn}
					class="w-full justify-start {paperFrame ? 'h-[38px] shrink-0' : ''}"
				/>
				<p class="text-xs text-text-faint {paperFrame ? 'h-9 leading-[18px]' : 'leading-5'}">
					Other LAN devices require the configured Bearer key. This setup screen does not collect or
					render secret key material.
				</p>
			</div>
		</li>
	</ol>

	<footer
		class="flex flex-wrap items-center justify-between gap-3 border-t border-hairline {paperFrame
			? 'h-[79px] shrink-0 px-8 py-5'
			: 'px-5 py-4 md:px-6'}"
	>
		<span class="font-mono text-2xs tracking-caps text-text-faint">
			{paperFrame ? 'AGPL-3.0 · LOOPBACK OPEN · REMOTE KEY · NO CLOUD' : 'LOOPBACK OPEN · NO CLOUD'}
		</span>
		<button
			type="button"
			class="inline-flex items-center gap-2 rounded-sm bg-primary font-semibold text-on-primary disabled:cursor-not-allowed disabled:opacity-45 {paperFrame
				? 'h-[38px] w-[174px] justify-center px-5 text-sm'
				: 'h-9 px-4 text-xs'}"
			disabled={!storageEvidence.canStartRecorder}
			title={storageEvidence.detail}
		>
			Start the recorder <ArrowRightIcon class="size-3.5" />
		</button>
	</footer>
</section>
