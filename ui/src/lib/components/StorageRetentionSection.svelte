<script lang="ts">
	import { capabilityActions } from '$lib/capability-actions';
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import {
		formatStorageBufferDuration,
		formatStorageDuration,
		mostSpecificDiskForPath,
		storageRetentionEvidence
	} from '$lib/storage-retention';
	import type { SanitizedConfig, ServerHealthResponse } from '$lib/types';
	import ArchiveIcon from '@lucide/svelte/icons/archive';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import HardDriveIcon from '@lucide/svelte/icons/hard-drive';
	import MemoryStickIcon from '@lucide/svelte/icons/memory-stick';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import TimerResetIcon from '@lucide/svelte/icons/timer-reset';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import StorageRetentionPaperFrame from './StorageRetentionPaperFrame.svelte';

	type Props = {
		config: SanitizedConfig;
		health: ServerHealthResponse | null;
		healthError?: string | null;
		paperFrame?: boolean;
		onedit: () => void;
	};

	let { config, health, healthError = null, paperFrame = false, onedit }: Props = $props();
	let evidence = $derived(storageRetentionEvidence(config, health));
	let diskUsedPercent = $derived(
		evidence.recordingDisk && evidence.recordingDisk.total_bytes > 0
			? Math.min(
					100,
					(evidence.recordingDisk.used_bytes / evidence.recordingDisk.total_bytes) * 100
				)
			: 0
	);
	let observedSpan = $derived(
		formatObservedSpan(evidence.oldestFootageAtMs, evidence.newestFootageAtMs)
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
		return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: value >= 100 ? 0 : 2 }).format(value)} ${units[unitIndex]}`;
	}

	function formatProjectedDays(days: number | null): string {
		if (days === null) return 'Needs bitrate data';
		return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(days)} days`;
	}

	function formatObservedTimestamp(timestampMs: number | null): string {
		if (timestampMs === null) return 'Not reported';
		return `${new Intl.DateTimeFormat(undefined, {
			dateStyle: 'medium',
			timeStyle: 'short',
			timeZone: 'UTC'
		}).format(timestampMs)} UTC`;
	}

	function formatObservedSpan(oldestMs: number | null, newestMs: number | null): string | null {
		if (oldestMs === null || newestMs === null || newestMs < oldestMs) return null;
		const days = (newestMs - oldestMs) / 86_400_000;
		if (days >= 1) {
			return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(days)} days of indexed footage observed.`;
		}
		return `${formatStorageDuration((newestMs - oldestMs) / 1_000)} of indexed footage observed.`;
	}

	function diskForPath(path: string) {
		return mostSpecificDiskForPath(path, health?.system.disks ?? []);
	}
</script>

{#if paperFrame}
	<StorageRetentionPaperFrame {config} {health} />
{:else}
	<section
		id="storage"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface [font-synthesis:none]"
		aria-labelledby="storage-retention-heading"
	>
		<header
			data-storage-band="heading"
			class="flex flex-wrap items-end justify-between gap-5 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-3xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">STORAGE & RETENTION</p>
				<h2 id="storage-retention-heading" class="mt-1 text-[28px] leading-[34px] font-semibold">
					Storage & retention
				</h2>
				<p class="mt-1 text-sm leading-[22px] text-text-muted">
					Memory buffering, writer rollover, archive capacity, and disk health are separate limits.
					This view shows what is configured and what the server can measure now.
				</p>
			</div>
			<div class="flex items-end gap-5">
				<div class="text-right">
					<p class="text-[32px] leading-9 font-bold text-primary-soft">
						{formatProjectedDays(evidence.projectedRetentionDays)}
					</p>
					<p class="font-mono text-2xs tracking-caps text-text-faint">
						PROJECTED AT CONFIGURED CAP
					</p>
					<p class="mt-1 text-2xs text-text-faint">
						{config.recording_estimate.known_streams} measured · {config.recording_estimate
							.unknown_streams} unmeasured
					</p>
				</div>
				<button
					type="button"
					class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
					onclick={onedit}
				>
					<PencilIcon class="size-3.5" /> Change storage
				</button>
			</div>
		</header>

		<div data-storage-band="capacity" class="border-b border-hairline bg-raised p-5">
			{#if evidence.recordingDisk}
				<div class="flex flex-wrap items-baseline justify-between gap-2">
					<h3 class="text-sm font-semibold">
						{evidence.recordingDisk.mount_point} · {evidence.recordingDisk.name} · {formatBytes(
							evidence.recordingDisk.total_bytes
						)}
					</h3>
					<p class="font-mono text-2xs text-text-muted">
						{formatBytes(evidence.recordingDisk.used_bytes)} USED · {formatBytes(
							evidence.recordingDisk.available_bytes
						)} FREE · {formatBytes(evidence.recordingDisk.total_bytes)} TOTAL
					</p>
				</div>
				<div
					class="mt-3 h-[22px] overflow-hidden rounded-xs bg-hairline"
					aria-label={`${diskUsedPercent.toFixed(1)} percent of recording disk used`}
				>
					<div class="h-full bg-primary" style:width={`${diskUsedPercent}%`}></div>
				</div>
				<div class="mt-3 flex flex-wrap gap-x-7 gap-y-2 text-xs text-text-muted">
					<span>Indexed fragments {formatBytes(evidence.indexedFragmentBytes)}</span>
					<span>Catalog {formatBytes(evidence.catalogBytes)}</span>
					<span>Thumbnails {evidence.eventThumbnailCount ?? 'Unavailable'}</span>
					<span
						>Configured cap {evidence.archive.limitBytes === null
							? 'Unlimited'
							: formatBytes(evidence.archive.limitBytes)}</span
					>
				</div>
			{:else}
				<div
					class="flex gap-2 rounded-sm border border-activity/45 bg-activity/5 px-3 py-3 text-xs leading-5 text-text-muted"
				>
					<TriangleAlertIcon class="mt-0.5 size-4 shrink-0 text-activity" />
					<span
						>{healthError ??
							'Recording-disk capacity is unavailable in the current health snapshot.'}</span
					>
				</div>
			{/if}
		</div>

		<div data-storage-band="tiers" class="grid border-b border-hairline lg:grid-cols-3">
			<article class="space-y-3 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div class="flex items-center justify-between gap-2">
					<span class="font-mono text-2xs tracking-caps text-primary-soft">TIER 1 · MEMORY</span>
					<MemoryStickIcon class="size-4 text-text-faint" />
				</div>
				<h3 class="text-lg font-semibold">Short-term buffer</h3>
				<div class="rounded-sm border border-hairline bg-raised px-3 py-2.5">
					<p class="font-mono text-2xs tracking-caps text-text-faint">BOUNDED BY DURATION</p>
					<p class="mt-1 text-sm font-medium">
						{formatStorageBufferDuration(evidence.shortTerm.durationSeconds)}
					</p>
				</div>
				<p class="text-xs leading-5 text-text-muted">
					Frames wait in memory before demand or the flush interval moves them to an active MP4
					writer. This tier consumes no disk allocation.
				</p>
			</article>

			<article class="space-y-3 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div class="flex items-center justify-between gap-2">
					<span class="font-mono text-2xs tracking-caps text-primary-soft"
						>TIER 2 · ACTIVE WRITER</span
					>
					<TimerResetIcon class="size-4 text-text-faint" />
				</div>
				<h3 class="text-lg font-semibold">Rolling MP4 segment</h3>
				<div class="rounded-sm border border-hairline bg-raised px-3 py-2.5">
					<p class="font-mono text-2xs tracking-caps text-text-faint">WRITER ROLLOVER</p>
					<p class="mt-1 text-sm font-medium">
						{formatStorageDuration(evidence.activeWriter.rolloverSeconds)}
					</p>
				</div>
				<p class="text-xs leading-5 text-text-muted">
					Flushes every {formatStorageDuration(evidence.activeWriter.flushSeconds)} with a {formatBytes(
						evidence.activeWriter.writeBufferBytes
					)} write buffer. This duration sizes active files; it is not retention age.
				</p>
			</article>

			<article class="space-y-3 p-5">
				<div class="flex items-center justify-between gap-2">
					<span class="font-mono text-2xs tracking-caps text-primary-soft">TIER 3 · ARCHIVE</span>
					<ArchiveIcon class="size-4 text-text-faint" />
				</div>
				<h3 class="text-lg font-semibold">Finalized recordings</h3>
				<div class="rounded-sm border border-hairline bg-raised px-3 py-2.5">
					<p class="font-mono text-2xs tracking-caps text-text-faint">SIZE CAP</p>
					<p class="mt-1 text-sm font-medium">
						{evidence.archive.limitBytes === null
							? 'Unlimited'
							: formatBytes(evidence.archive.limitBytes)}
					</p>
				</div>
				<p class="text-xs leading-5 text-text-muted">
					The archive scans dated recording directories and removes the oldest until it is within
					the cap. A zero cap leaves it unbounded.
				</p>
			</article>
		</div>

		<section
			data-storage-band="locations"
			class="border-b border-hairline p-5"
			aria-labelledby="storage-locations-heading"
		>
			<header class="flex flex-wrap items-center justify-between gap-3 pb-3">
				<h3 id="storage-locations-heading" class="flex items-center gap-2 text-lg font-semibold">
					<HardDriveIcon class="size-4" /> Where it is written
				</h3>
				<CapabilityGate
					{...capabilityActions.addOffsiteArchive}
					class="h-[30px] min-h-[30px] px-3 text-[11px]"
				/>
			</header>
			<div
				class="hidden border-b border-hairline-strong pb-2 font-mono text-2xs tracking-caps text-text-faint sm:grid sm:grid-cols-[minmax(0,2fr)_minmax(8rem,0.8fr)_minmax(7rem,0.6fr)_minmax(6rem,0.5fr)] sm:gap-4"
			>
				<span>PATH</span><span>HOLDS</span><span>DEVICE</span><span>STATUS</span>
			</div>
			{#each [[evidence.activeWriter.path, 'Active MP4 files'], [evidence.archive.path, 'Finalized archive'], [config.storage.recording_catalog_path, 'Catalog and events'], [config.storage.event_thumbnail_path, 'Event thumbnails']] as row (`${row[0]}-${row[1]}`)}
				{@const disk = diskForPath(row[0])}
				<div
					class="grid gap-1 border-b border-hairline py-3 text-xs last:border-b-0 sm:grid-cols-[minmax(0,2fr)_minmax(8rem,0.8fr)_minmax(7rem,0.6fr)_minmax(6rem,0.5fr)] sm:items-center sm:gap-4"
				>
					<span class="font-mono break-all">{row[0]}</span>
					<span class="text-text-muted">{row[1]}</span>
					<span class="text-text-muted">{disk?.name ?? 'Not mapped'}</span>
					<span
						class="flex items-center gap-2 font-mono text-2xs {disk
							? 'text-healthy'
							: 'text-text-faint'}"
					>
						<span class="size-1.5 rounded-full {disk ? 'bg-healthy' : 'bg-text-faint'}"></span>
						{disk ? 'Observed' : 'Path only'}
					</span>
				</div>
			{/each}
		</section>

		<div data-storage-band="policy" class="grid lg:grid-cols-2">
			<article class="space-y-3 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<h3 class="text-lg font-semibold">When the archive reaches its cap</h3>
				<div class="flex gap-3 rounded-sm border border-primary bg-primary/5 p-3">
					<span class="mt-0.5 size-4 shrink-0 rounded-full border-4 border-primary"></span>
					<div>
						<p class="text-sm font-medium">Prune the oldest dated recordings</p>
						<p class="mt-1 text-xs leading-5 text-text-muted">
							Fixed engine behavior. Health becomes critical below {evidence.diskWarningThresholdPercent}%
							free.
						</p>
					</div>
				</div>
				<div class="flex items-center justify-between gap-4 border-t border-hairline pt-3 text-sm">
					<span>Actual oldest footage</span>
					{#if evidence.oldestFootageAtMs === null}
						<span class="text-right font-mono text-xs text-text-faint">Not reported</span>
					{:else}
						<time
							datetime={new Date(evidence.oldestFootageAtMs).toISOString()}
							class="text-right font-mono text-xs text-text-faint"
						>
							{formatObservedTimestamp(evidence.oldestFootageAtMs)}
						</time>
					{/if}
				</div>
				{#if observedSpan}
					<p class="text-xs leading-5 text-text-muted">{observedSpan}</p>
				{:else}
					<p class="text-xs leading-5 text-text-muted">
						Projected retention is never presented as observed history.
					</p>
				{/if}
			</article>

			<article class="space-y-3 p-5">
				<div class="flex items-center justify-between gap-3">
					<h3 class="text-lg font-semibold">Camera recording policy</h3>
					<span class="font-mono text-2xs text-healthy">CONFIGURED PER CAMERA</span>
				</div>
				{#each [['Recording modes', 'Off · Sub · Main · Both'], ['Event boost', 'Sub → Main → Sub'], ['Pinned recordings', 'Not available']] as row (row[0])}
					<div
						class="flex min-h-10 items-center justify-between gap-4 border-b border-hairline text-sm"
					>
						<span>{row[0]}</span><span class="font-mono text-xs text-text-faint">{row[1]}</span>
					</div>
				{/each}
				<div class="flex gap-2 pt-1 text-xs leading-5 text-text-muted">
					<DatabaseIcon class="mt-0.5 size-4 shrink-0 text-text-faint" /> Indexed fragment bytes cover
					cataloged media; disk use can also include active files and unrelated data.
				</div>
			</article>
		</div>
	</section>
{/if}
