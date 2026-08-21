<script lang="ts">
	import { capabilityActions } from '$lib/capability-actions';
	import { storageRetentionEvidence } from '$lib/storage-retention';
	import type { SanitizedConfig, ServerHealthResponse } from '$lib/types';
	import CapabilityGate from './CapabilityGate.svelte';
	import DesktopPaperRail from './DesktopPaperRail.svelte';
	import SettingsPaperAnchorRail from './SettingsPaperAnchorRail.svelte';

	type Props = {
		config: SanitizedConfig;
		health: ServerHealthResponse | null;
	};

	let { config, health }: Props = $props();
	let evidence = $derived(storageRetentionEvidence(config, health));
	let diskUsedPercent = $derived(
		evidence.recordingDisk && evidence.recordingDisk.total_bytes > 0
			? Math.min(
					100,
					(evidence.recordingDisk.used_bytes / evidence.recordingDisk.total_bytes) * 100
				)
			: 0
	);

	function formatBytes(bytes: number | null): string {
		if (bytes === null) return 'Unavailable';
		if (bytes < 1_000) return `${bytes} B`;
		const units = ['kB', 'MB', 'GB', 'TB', 'PB'];
		let value = bytes / 1_000;
		let unitIndex = 0;
		while (value >= 1_000 && unitIndex < units.length - 1) {
			value /= 1_000;
			unitIndex += 1;
		}
		return `${new Intl.NumberFormat('en-US', { maximumFractionDigits: value >= 100 ? 0 : 2 }).format(value)} ${units[unitIndex]}`;
	}

	function formatDuration(seconds: number): string {
		if (seconds < 60) return `${seconds} seconds`;
		if (seconds % 3600 === 0) {
			const hours = seconds / 3600;
			return `${hours} ${hours === 1 ? 'hour' : 'hours'}`;
		}
		if (seconds % 60 === 0) return `${seconds / 60} minutes`;
		return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
	}

	function formatProjectedDays(days: number | null): string {
		return days === null
			? 'Unavailable'
			: `${new Intl.NumberFormat('en-US', { maximumFractionDigits: 1 }).format(days)} days`;
	}
</script>

<section
	data-storage-retention-paper-frame
	class="flex h-[1163px] w-[1440px] overflow-hidden rounded-lg border border-hairline bg-surface [font-synthesis:none]"
	aria-label="Storage and retention evidence"
>
	<DesktopPaperRail />

	<div class="flex h-[1161px] w-[1374px] shrink-0 flex-col">
		<header
			class="flex h-[52px] w-[1374px] shrink-0 items-center justify-between border-b border-hairline px-5"
		>
			<div class="flex items-baseline gap-3">
				<h2 class="text-base leading-5 font-semibold">Settings</h2>
				<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-muted">
					SERVER-WIDE · ADMINISTRATOR ONLY
				</p>
			</div>
			<span class="font-mono text-2xs leading-[14px] text-text-faint"
				>OBSERVED VALUES · EDIT IN RUNTIME DIALOG</span
			>
		</header>

		<div class="flex h-[1109px] shrink-0">
			<SettingsPaperAnchorRail active="storage" />
			<div class="flex h-[1109px] w-[1134px] shrink-0 flex-col gap-7 px-8 py-7">
				<section
					data-storage-band="heading"
					class="flex h-[84px] w-[1070px] shrink-0 items-end justify-between"
					aria-labelledby="paper-storage-heading"
				>
					<div class="flex h-[84px] w-[720px] shrink-0 flex-col gap-1.5">
						<h1 id="paper-storage-heading" class="text-[28px] leading-[34px] font-semibold">
							Storage & retention
						</h1>
						<p class="text-sm leading-[22px] text-text-muted">
							Memory buffering, writer rollover, archive cap, and measured disk capacity are
							distinct. Observed oldest-footage history is not exposed.
						</p>
					</div>
					<div class="flex h-[58px] shrink-0 flex-col items-end gap-0.5">
						<p class="text-[40px] leading-[42px] font-bold text-primary-soft">
							{formatProjectedDays(evidence.projectedRetentionDays)}
						</p>
						<p class="font-mono text-2xs leading-[14px] tracking-[0.1em] text-text-faint">
							PROJECTED AT CONFIGURED CAP
						</p>
					</div>
				</section>

				<section
					data-storage-band="capacity"
					class="flex h-32 w-[1070px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-raised p-5"
					aria-label="Measured recording disk capacity"
				>
					<div class="flex h-5 shrink-0 items-baseline justify-between">
						<h2 class="text-base leading-5 font-semibold">
							{evidence.recordingDisk
								? `${evidence.recordingDisk.mount_point} — ${formatBytes(evidence.recordingDisk.total_bytes)}`
								: 'Recording disk unavailable'}
						</h2>
						<p class="font-mono text-xs-plus leading-4 text-text-muted">
							{evidence.recordingDisk
								? `${formatBytes(evidence.recordingDisk.used_bytes)} USED · ${formatBytes(evidence.recordingDisk.available_bytes)} FREE`
								: 'CAPACITY UNAVAILABLE'}
						</p>
					</div>
					<div class="flex h-[22px] w-[1030px] shrink-0 overflow-hidden rounded-xs bg-hairline">
						<span class="h-[22px] bg-primary" style:width={`${diskUsedPercent}%`}></span>
					</div>
					<div class="flex h-4 shrink-0 gap-7 text-xs leading-4 text-text-muted">
						<span
							>Disk used {evidence.recordingDisk
								? formatBytes(evidence.recordingDisk.used_bytes)
								: 'Unavailable'}</span
						>
						<span>Indexed fragments {formatBytes(evidence.indexedFragmentBytes)}</span>
						<span>Catalog {formatBytes(evidence.catalogBytes)}</span>
						<span>Thumbnails {evidence.eventThumbnailCount ?? 'Unavailable'}</span>
						<span
							>Free {evidence.recordingDisk
								? formatBytes(evidence.recordingDisk.available_bytes)
								: 'Unavailable'}</span
						>
					</div>
				</section>

				<section
					data-storage-band="tiers"
					class="flex h-[235px] w-[1070px] shrink-0 gap-5"
					aria-label="Storage tiers"
				>
					<article
						class="flex h-[235px] w-[344px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-[18px]"
					>
						<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-primary-soft">
							TIER 1 · MEMORY
						</p>
						<h2 class="text-lg leading-[22px] font-semibold">Short-term buffer</h2>
						<div class="flex h-14 shrink-0 flex-col gap-1.5">
							<p class="font-mono text-2xs tracking-[0.12em] text-text-faint">
								BOUNDED BY DURATION
							</p>
							<div
								class="flex h-9 items-center justify-between rounded-sm border border-hairline-strong bg-raised px-3"
							>
								<span class="text-sm">{evidence.shortTerm.durationSeconds} seconds</span><span
									class="font-mono text-2xs text-text-faint">IN MEMORY</span
								>
							</div>
						</div>
						<p class="text-[13px] leading-[21px] text-text-muted">
							Frames remain in memory until demand or the flush cadence moves them to an active
							writer.
						</p>
					</article>

					<article
						class="flex h-[235px] w-[343px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-[18px]"
					>
						<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-primary-soft">
							TIER 2 · ACTIVE WRITER
						</p>
						<h2 class="text-lg leading-[22px] font-semibold">Rolling MP4 segment</h2>
						<div class="flex h-14 shrink-0 flex-col gap-1.5">
							<p class="font-mono text-2xs tracking-[0.12em] text-text-faint">WRITER ROLLOVER</p>
							<div
								class="flex h-9 items-center justify-between rounded-sm border border-primary bg-raised px-3"
							>
								<span class="text-sm">{formatDuration(evidence.activeWriter.rolloverSeconds)}</span
								><span class="font-mono text-2xs text-activity">CONFIGURED</span>
							</div>
						</div>
						<p class="text-[13px] leading-[21px] text-text-muted">
							Flushes every {formatDuration(evidence.activeWriter.flushSeconds)} with a {formatBytes(
								evidence.activeWriter.writeBufferBytes
							)} buffer. This sizes active files, not retention age.
						</p>
					</article>

					<article
						class="flex h-[235px] w-[343px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-[18px]"
					>
						<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-primary-soft">
							TIER 3 · ARCHIVE
						</p>
						<h2 class="text-lg leading-[22px] font-semibold">Finalized recordings</h2>
						<div class="flex h-14 shrink-0 flex-col gap-1.5">
							<p class="font-mono text-2xs tracking-[0.12em] text-text-faint">SIZE CAP</p>
							<div
								class="flex h-9 items-center justify-between rounded-sm border border-hairline-strong bg-raised px-3"
							>
								<span class="text-sm">{formatBytes(evidence.archive.limitBytes)}</span><span
									class="font-mono text-2xs text-text-faint">CONFIGURED</span
								>
							</div>
						</div>
						<p class="text-[13px] leading-[21px] text-text-muted">
							Dated recording directories are pruned oldest-first until the archive fits its
							configured cap.
						</p>
					</article>
				</section>

				<section
					data-storage-band="locations"
					class="flex h-54 w-[1070px] shrink-0 flex-col"
					aria-labelledby="storage-locations-heading"
				>
					<header class="flex h-[42px] shrink-0 items-start justify-between pb-3">
						<h2 id="storage-locations-heading" class="text-lg leading-[22px] font-semibold">
							Where it is written
						</h2>
						<CapabilityGate
							{...capabilityActions.addOffsiteArchive}
							class="h-[30px] min-h-[30px] px-3 text-[11px]"
						/>
					</header>
					<div
						class="flex h-[30px] shrink-0 items-center border-b border-hairline-strong font-mono text-2xs tracking-[0.14em] text-text-faint"
					>
						<span class="w-[340px]">PATH</span><span class="w-[200px]">DEVICE</span><span
							class="w-[130px]">CAPACITY</span
						><span class="w-[130px]">FREE</span><span class="w-[190px]">HOLDS</span><span
							>STATUS</span
						>
					</div>
					{#each [[evidence.activeWriter.path, evidence.recordingDisk?.name ?? 'Device unavailable', evidence.recordingDisk ? formatBytes(evidence.recordingDisk.total_bytes) : '—', evidence.recordingDisk ? formatBytes(evidence.recordingDisk.available_bytes) : '—', 'Active MP4 files', 'Configured'], [evidence.archive.path, evidence.recordingDisk?.name ?? 'Device unavailable', evidence.recordingDisk ? formatBytes(evidence.recordingDisk.total_bytes) : '—', evidence.recordingDisk ? formatBytes(evidence.recordingDisk.available_bytes) : '—', 'Finalized archive', 'Configured'], [config.storage.recording_catalog_path, 'Device not mapped', '—', '—', 'Catalog and event index', 'Path only']] as row (`${row[0]}-${row[4]}`)}
						<div class="flex h-12 shrink-0 items-center border-b border-hairline text-[13px]">
							<span class="w-[340px] shrink-0 font-mono">{row[0]}</span><span
								class="w-[200px] shrink-0 text-text-muted">{row[1]}</span
							><span class="w-[130px] shrink-0 font-mono">{row[2]}</span><span
								class="w-[130px] shrink-0 font-mono">{row[3]}</span
							><span class="w-[190px] shrink-0 text-text-muted">{row[4]}</span><span
								class="font-mono text-xs text-text-faint">{row[5]}</span
							>
						</div>
					{/each}
				</section>

				<section
					data-storage-band="policy"
					class="flex h-[278px] w-[1070px] shrink-0 gap-5"
					aria-label="Storage policy evidence"
				>
					<article
						class="flex h-[278px] w-[525px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-[18px]"
					>
						<h2 class="text-lg leading-[22px] font-semibold">When the disk fills</h2>
						<div
							class="flex min-h-[62px] items-center gap-3 rounded-sm border border-primary bg-primary/5 p-3"
						>
							<span class="size-4 shrink-0 rounded-full border-4 border-primary"></span>
							<div>
								<p class="text-sm leading-[18px] font-medium">Prune the oldest dated recordings</p>
								<p class="mt-0.5 text-xs leading-4 text-text-muted">
									Current engine behavior · not a selectable policy
								</p>
							</div>
						</div>
						<div
							class="flex min-h-[62px] items-center gap-3 rounded-sm border border-hairline-strong p-3 opacity-55"
						>
							<span class="size-4 shrink-0 rounded-full border border-hairline-strong"></span>
							<div>
								<p class="text-sm leading-[18px] font-medium">Stop recording when full</p>
								<p class="mt-0.5 text-xs leading-4 text-text-muted">
									No configuration field or runtime command exists.
								</p>
							</div>
						</div>
						<div class="flex h-10 items-center justify-between pt-1">
							<div>
								<p class="text-sm">Disk warning threshold</p>
								<p class="text-xs text-text-faint">Fixed health behavior</p>
							</div>
							<span
								class="rounded-sm border border-hairline-strong bg-raised px-3 py-2 font-mono text-[13px]"
								>{evidence.diskWarningThresholdPercent}% free</span
							>
						</div>
					</article>

					<article
						class="flex h-[278px] w-[525px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-[18px]"
					>
						<div class="flex items-center justify-between">
							<h2 class="text-lg leading-[22px] font-semibold">Camera retention evidence</h2>
							<span class="font-mono text-2xs text-text-faint">UNAVAILABLE</span>
						</div>
						{#each ['Recording mode', 'Retention override', 'Pinned recordings'] as row (row)}
							<div class="flex h-10 shrink-0 items-center justify-between border-b border-hairline">
								<span class="text-sm">{row}</span><span class="font-mono text-xs text-text-faint"
									>Not returned</span
								>
							</div>
						{/each}
						<p class="text-[13px] leading-[21px] text-text-muted">
							Actual oldest-footage time is also unavailable. Projected retention is never presented
							as observed history.
						</p>
					</article>
				</section>
			</div>
		</div>
	</div>
</section>
