<script lang="ts">
	import { capabilityActions } from '$lib/capability-actions';
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import {
		formatStorageBufferDuration,
		formatStorageDuration,
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
		if (days === null) return 'Unavailable';
		return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(days)} days`;
	}
</script>

{#if paperFrame}
	<StorageRetentionPaperFrame {config} {health} />
{:else}
	<section
		id="storage"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
		aria-labelledby="storage-retention-heading"
	>
		<header
			class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">STORAGE & RETENTION</p>
				<h2 id="storage-retention-heading" class="mt-1 text-xl font-semibold">
					Storage & retention
				</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">
					The storage engine buffers in memory, rolls active MP4 writers, then prunes the oldest
					dated recordings when the archive reaches its configured cap.
				</p>
			</div>
			<div class="flex items-end gap-4">
				<div class="text-right">
					<p class="text-2xl font-semibold text-primary-soft">
						{formatProjectedDays(evidence.projectedRetentionDays)}
					</p>
					<p class="font-mono text-2xs tracking-caps text-text-faint">
						PROJECTED AT CONFIGURED CAP
					</p>
				</div>
				<button
					type="button"
					class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
					onclick={onedit}
				>
					<PencilIcon class="size-3.5" /> Edit runtime storage
				</button>
			</div>
		</header>

		<div class="border-b border-hairline bg-raised p-5">
			{#if evidence.recordingDisk}
				<div class="flex flex-wrap items-baseline justify-between gap-2">
					<h3 class="text-sm font-semibold">
						{evidence.recordingDisk.mount_point} · {evidence.recordingDisk.name}
					</h3>
					<p class="font-mono text-2xs text-text-muted">
						{formatBytes(evidence.recordingDisk.used_bytes)} USED · {formatBytes(
							evidence.recordingDisk.available_bytes
						)} FREE · {formatBytes(evidence.recordingDisk.total_bytes)} TOTAL
					</p>
				</div>
				<div
					class="mt-3 h-5 overflow-hidden rounded-xs bg-hairline"
					aria-label={`${diskUsedPercent.toFixed(1)} percent of recording disk used`}
				>
					<div class="h-full bg-primary" style:width={`${diskUsedPercent}%`}></div>
				</div>
				<div class="mt-3 flex flex-wrap gap-x-6 gap-y-2 text-xs text-text-muted">
					<span>Indexed fragments {formatBytes(evidence.indexedFragmentBytes)}</span>
					<span>Catalog {formatBytes(evidence.catalogBytes)}</span>
					<span>Event thumbnails {evidence.eventThumbnailCount ?? 'Unavailable'}</span>
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

		<div class="grid border-b border-hairline lg:grid-cols-3">
			<article class="space-y-3 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div class="flex items-center justify-between gap-2">
					<span class="font-mono text-2xs tracking-caps text-primary-soft">TIER 1 · MEMORY</span>
					<MemoryStickIcon class="size-4 text-text-faint" />
				</div>
				<h3 class="text-base font-semibold">Short-term buffer</h3>
				<div class="rounded-sm border border-hairline bg-raised px-3 py-2.5">
					<p class="font-mono text-2xs tracking-caps text-text-faint">TIME WINDOW</p>
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
				<h3 class="text-base font-semibold">Rolling MP4 segment</h3>
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
				<h3 class="text-base font-semibold">Finalized recordings</h3>
				<div class="rounded-sm border border-hairline bg-raised px-3 py-2.5">
					<p class="font-mono text-2xs tracking-caps text-text-faint">SIZE CAP</p>
					<p class="mt-1 text-sm font-medium">{formatBytes(evidence.archive.limitBytes)}</p>
				</div>
				<p class="text-xs leading-5 text-text-muted">
					The archive scans dated recording directories and removes the oldest until it is within
					the cap. A zero cap leaves it unbounded.
				</p>
			</article>
		</div>

		<div class="grid lg:grid-cols-[1.15fr_0.85fr]">
			<div class="space-y-3 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<h3 class="flex items-center gap-2 text-base font-semibold">
					<HardDriveIcon class="size-4" /> Where it is written
				</h3>
				<dl class="divide-y divide-hairline border-y border-hairline text-xs">
					<div class="grid gap-1 py-3 sm:grid-cols-[9rem_minmax(0,1fr)]">
						<dt class="text-text-muted">Active MP4 files</dt>
						<dd class="font-mono break-all">{evidence.activeWriter.path}</dd>
					</div>
					<div class="grid gap-1 py-3 sm:grid-cols-[9rem_minmax(0,1fr)]">
						<dt class="text-text-muted">Finalized archive</dt>
						<dd class="font-mono break-all">{evidence.archive.path}</dd>
					</div>
					<div class="grid gap-1 py-3 sm:grid-cols-[9rem_minmax(0,1fr)]">
						<dt class="text-text-muted">Recording catalog</dt>
						<dd class="font-mono break-all">{config.storage.recording_catalog_path}</dd>
					</div>
					<div class="grid gap-1 py-3 sm:grid-cols-[9rem_minmax(0,1fr)]">
						<dt class="text-text-muted">Event thumbnails</dt>
						<dd class="font-mono break-all">{config.storage.event_thumbnail_path}</dd>
					</div>
				</dl>
				<div class="rounded-sm border border-hairline bg-raised px-3 py-3">
					<p class="font-mono text-2xs tracking-caps text-text-faint">ACTUAL OLDEST FOOTAGE</p>
					<p class="mt-1 text-xs leading-5 text-text-muted">
						Unavailable. The config and health responses do not expose the oldest catalog timestamp,
						so projected retention is never labeled as observed history.
					</p>
				</div>
			</div>

			<div class="space-y-4 p-5">
				<div>
					<h3 class="text-base font-semibold">When the archive reaches its cap</h3>
					<div class="mt-2 rounded-sm border border-primary bg-primary/5 px-3 py-3">
						<p class="text-sm font-medium">Prune the oldest dated recordings</p>
						<p class="mt-1 text-xs leading-5 text-text-muted">
							This is the current engine behavior, not a selectable policy. Health raises a critical
							issue when the recording disk falls below {evidence.diskWarningThresholdPercent}%
							free.
						</p>
					</div>
				</div>
				<div>
					<p class="font-mono text-2xs tracking-caps text-text-faint">PER-CAMERA RETENTION</p>
					<p class="mt-1 text-xs leading-5 text-text-muted">
						Unavailable. Camera settings expose no recording mode, retention override, or pin
						policy.
					</p>
				</div>
				<div>
					<p class="font-mono text-2xs tracking-caps text-text-faint">ADDITIONAL LOCATIONS</p>
					<div class="mt-2">
						<CapabilityGate {...capabilityActions.addOffsiteArchive} class="w-full justify-start" />
					</div>
				</div>
				<div
					class="flex gap-2 rounded-sm border border-hairline bg-raised px-3 py-3 text-xs leading-5 text-text-muted"
				>
					<DatabaseIcon class="mt-0.5 size-4 shrink-0 text-text-faint" /> Indexed fragment bytes measure
					cataloged media only; disk usage can include active files and unrelated data.
				</div>
			</div>
		</div>
	</section>
{/if}
