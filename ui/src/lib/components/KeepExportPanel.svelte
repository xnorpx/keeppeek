<script lang="ts">
	import { resolve } from '$app/paths';
	import { untrack } from 'svelte';
	import { capabilityActions } from '$lib/capability-actions';
	import { useCapabilityState } from '$lib/capability-context';
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import { useControlClient } from '$lib/control-context';
	import type { MediaExportJob } from '$lib/control-client';
	import {
		classifyExportCandidates,
		createExportRange,
		updateExportRange,
		type ExportCandidateMatch,
		type ExportRange
	} from '$lib/keep-modes';
	import type { RecordingEvent, RecordingSegment } from '$lib/types';
	import CheckCircleIcon from '@lucide/svelte/icons/circle-check';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import HardDriveIcon from '@lucide/svelte/icons/hard-drive';
	import InfoIcon from '@lucide/svelte/icons/info';
	import LayersIcon from '@lucide/svelte/icons/layers';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import XIcon from '@lucide/svelte/icons/x';

	type Props = {
		sourceId: string;
		sourceName: string;
		segment: RecordingSegment | null;
		bitrateKbps: number | null;
		jobPresentation?: boolean;
		paperFrame?: boolean;
		rangeStartMs?: number | null;
		rangeEndMs?: number | null;
		event?: RecordingEvent | null;
	};
	type AvailabilitySection = { kind: 'available' | 'missing'; percent: number };
	type TrimTarget = { startMs: number; endMs: number; label: string };

	let {
		sourceId,
		sourceName,
		segment,
		bitrateKbps,
		jobPresentation = false,
		paperFrame = false,
		rangeStartMs = null,
		rangeEndMs = null,
		event = null
	}: Props = $props();
	const controlClient = useControlClient();
	const capabilities = useCapabilityState();
	const initialRange = untrack(() => createExportRange(segment, bitrateKbps));
	let range = $state.raw<ExportRange | null>(initialRange);
	let burnInTimestamp = $state(false);
	let allowPartialDraft = $state(false);
	let selectedJob = $state.raw<MediaExportJob | null>(null);
	let exportHistory = $state.raw<MediaExportJob[]>([]);
	let submitting = $state(false);
	let downloading = $state(false);
	let operationError = $state<string | null>(null);
	let loadedExportKey: string | null = null;
	let appliedRangeOverride = '';
	let exportSupported = $derived(capabilities.supports(capabilityActions.createExport.capability));
	let candidateMatch = $derived(exportCandidates(allowPartialDraft));
	let job = $derived(selectedJob ?? candidateMatch.exactActive);
	let partialSections = $derived(
		job?.status === 'partial'
			? jobPresentation
				? [
						{ kind: 'available' as const, percent: (118 / 304) * 100 },
						{ kind: 'missing' as const, percent: (52 / 304) * 100 },
						{ kind: 'available' as const, percent: (134 / 304) * 100 }
					]
				: availabilitySections(job)
			: []
	);
	let trimTarget = $derived(job?.status === 'partial' ? exportTrimTarget(job) : null);

	$effect(() => {
		const overrideKey = `${rangeStartMs ?? ''}:${rangeEndMs ?? ''}`;
		if (overrideKey === appliedRangeOverride) return;
		appliedRangeOverride = overrideKey;
		const currentRange = untrack(() => range);
		if (!currentRange) return;
		range = updateExportRange(
			currentRange,
			rangeStartMs ?? currentRange.startMs,
			rangeEndMs ?? currentRange.endMs,
			bitrateKbps
		);
	});

	$effect(() => {
		const exportKey = segment ? `${sourceId}:${segment.stream}` : null;
		if (!exportSupported || exportKey === null || loadedExportKey === exportKey) return;
		loadedExportKey = exportKey;
		let active = true;
		void controlClient.listExports().then(
			(jobs) => {
				if (!active) return;
				exportHistory = jobs;
				if (jobPresentation) selectedJob = jobs[0] ?? null;
			},
			(cause: unknown) => {
				if (active) operationError = errorMessage(cause, 'Export jobs could not be loaded.');
			}
		);
		return () => {
			active = false;
		};
	});

	$effect(() => {
		const jobId = job?.status === 'running' ? job.id : null;
		if (jobId === null) return;
		const timeout = setTimeout(() => {
			void controlClient.getExport(jobId).then(
				(nextJob) => {
					if (job?.id === jobId) rememberJob(nextJob);
				},
				(cause: unknown) => {
					if (job?.id === jobId) operationError = errorMessage(cause, 'Export status was lost.');
				}
			);
		}, 750);
		return () => clearTimeout(timeout);
	});

	function formatTimeInput(timestampMs: number): string {
		return new Date(timestampMs).toISOString().slice(11, 19);
	}

	function formatDuration(durationMs: number, padSeconds = false): string {
		const totalSeconds = Math.round(durationMs / 1_000);
		const minutes = Math.floor(totalSeconds / 60);
		const seconds = totalSeconds % 60;
		return minutes > 0
			? `${minutes}m ${padSeconds ? seconds.toString().padStart(2, '0') : seconds}s`
			: `${seconds}s`;
	}

	function formatBytes(bytes: number | null): string {
		if (bytes === null) return 'Not reported';
		if (bytes >= 1_000_000_000) return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
		if (paperFrame) return `${Math.round(bytes / 1_000_000)} MB`;
		return `${Math.max(0.1, bytes / 1_000_000).toFixed(1)} MB`;
	}

	function formatJobBytes(bytes: number | null): string {
		if (bytes === null) return 'Not reported';
		if (bytes >= 1_000_000_000) return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
		if (bytes >= 1_000_000) return `${Math.round(bytes / 1_000_000)} MB`;
		if (bytes >= 1_000) return `${Math.round(bytes / 1_000)} KB`;
		return `${bytes} B`;
	}

	function formatDate(timestampMs: number): string {
		return new Intl.DateTimeFormat('en-GB', {
			day: 'numeric',
			month: 'short',
			timeZone: 'UTC'
		}).format(timestampMs);
	}

	function formatTime(timestampMs: number): string {
		return new Date(timestampMs).toISOString().slice(11, 19);
	}

	function formatRange(startMs: number, endMs: number): string {
		return `${formatDate(startMs)} ${formatTime(startMs)} → ${formatTime(endMs)}`;
	}

	function formatChecksum(checksum: string | null): string {
		if (!checksum) return 'Not reported';
		return checksum.length > 16 ? `${checksum.slice(0, 8)} · · · ${checksum.slice(-8)}` : checksum;
	}

	function formatExpiry(expiresAtMs: number | null): string {
		if (expiresAtMs === null) return 'Not reported';
		const remainingMinutes = Math.floor((expiresAtMs - Date.now()) / 60_000);
		if (remainingMinutes <= 0) return 'Expired';
		const days = Math.floor(remainingMinutes / (24 * 60));
		const hours = Math.floor((remainingMinutes % (24 * 60)) / 60);
		const minutes = remainingMinutes % 60;
		if (days > 0) return `in ${days}d ${hours}h`;
		return `in ${hours}h ${minutes}m`;
	}

	function missingDuration(jobValue: MediaExportJob): number {
		return jobValue.missingRanges.reduce(
			(total, missing) => total + Math.max(0, missing.endMs - missing.startMs),
			0
		);
	}

	function availabilitySections(jobValue: MediaExportJob): AvailabilitySection[] {
		const durationMs = jobValue.requestedEndMs - jobValue.requestedStartMs;
		if (durationMs <= 0) return [];
		const sections: AvailabilitySection[] = [];
		let cursorMs = jobValue.requestedStartMs;
		for (const missing of jobValue.missingRanges.toSorted(
			(left, right) => left.startMs - right.startMs
		)) {
			const startMs = Math.max(cursorMs, missing.startMs);
			const endMs = Math.min(jobValue.requestedEndMs, missing.endMs);
			if (startMs > cursorMs) {
				sections.push({ kind: 'available', percent: ((startMs - cursorMs) / durationMs) * 100 });
			}
			if (endMs > startMs) {
				sections.push({ kind: 'missing', percent: ((endMs - startMs) / durationMs) * 100 });
			}
			cursorMs = Math.max(cursorMs, endMs);
		}
		if (cursorMs < jobValue.requestedEndMs) {
			sections.push({
				kind: 'available',
				percent: ((jobValue.requestedEndMs - cursorMs) / durationMs) * 100
			});
		}
		return sections;
	}

	function exportTrimTarget(jobValue: MediaExportJob): TrimTarget | null {
		const missing = jobValue.missingRanges[0];
		if (!missing) return null;
		if (missing.endMs < jobValue.requestedEndMs) {
			return {
				startMs: missing.endMs,
				endMs: jobValue.requestedEndMs,
				label: formatTime(missing.endMs)
			};
		}
		if (missing.startMs > jobValue.requestedStartMs) {
			return {
				startMs: jobValue.requestedStartMs,
				endMs: missing.startMs,
				label: formatTime(missing.startMs)
			};
		}
		return null;
	}

	function errorMessage(cause: unknown, fallback: string): string {
		return cause instanceof Error ? cause.message : fallback;
	}

	function exportCandidates(allowPartial: boolean): ExportCandidateMatch<MediaExportJob> {
		if (!range || !segment) return { exactActive: null, exactReady: null, related: [] };
		return classifyExportCandidates(exportHistory, {
			sourceId,
			streamId: segment.stream,
			startMs: range.startMs,
			endMs: range.endMs,
			allowPartial,
			burnInTimestamp
		});
	}

	function rememberJob(nextJob: MediaExportJob): void {
		selectedJob = nextJob;
		exportHistory = [nextJob, ...exportHistory.filter((candidate) => candidate.id !== nextJob.id)];
	}

	async function createJob(allowPartial = allowPartialDraft, createFresh = false): Promise<void> {
		if (!range || !segment || !exportSupported || submitting) return;
		allowPartialDraft = allowPartial;
		const matches = exportCandidates(allowPartial);
		if (matches.exactActive) {
			rememberJob(matches.exactActive);
			return;
		}
		if (!createFresh && (matches.exactReady || matches.related.length > 0)) {
			selectedJob = null;
			return;
		}
		submitting = true;
		operationError = null;
		try {
			rememberJob(
				await controlClient.createExport({
					sourceId,
					streamId: segment.stream,
					startMs: range.startMs,
					endMs: range.endMs,
					allowPartial,
					burnInTimestamp,
					event: event ?? undefined
				})
			);
		} catch (cause) {
			operationError = errorMessage(cause, 'The export could not be created.');
		} finally {
			submitting = false;
		}
	}

	async function cancelJob(): Promise<void> {
		if (!job || !exportSupported || submitting) return;
		submitting = true;
		operationError = null;
		try {
			rememberJob(await controlClient.cancelExport(job.id));
		} catch (cause) {
			operationError = errorMessage(cause, 'The export could not be cancelled.');
		} finally {
			submitting = false;
		}
	}

	async function retryJob(): Promise<void> {
		if (!job || !exportSupported || submitting) return;
		submitting = true;
		operationError = null;
		try {
			rememberJob(await controlClient.retryExport(job.id));
		} catch (cause) {
			operationError = errorMessage(cause, 'The export could not be retried.');
		} finally {
			submitting = false;
		}
	}

	async function downloadJob(): Promise<void> {
		if (!job || !exportSupported || downloading) return;
		downloading = true;
		operationError = null;
		try {
			const download = await controlClient.downloadExport(job.id);
			const url = URL.createObjectURL(download.blob);
			const anchor = document.createElement('a');
			anchor.href = url;
			anchor.download = download.job.fileName ?? 'keeppeek-export.mp4';
			document.body.append(anchor);
			anchor.click();
			anchor.remove();
			setTimeout(() => URL.revokeObjectURL(url), 0);
		} catch (cause) {
			operationError = errorMessage(cause, 'The export could not be downloaded.');
		} finally {
			downloading = false;
		}
	}

	function trimRange(target: TrimTarget): void {
		if (!range) return;
		range = updateExportRange(range, target.startMs, target.endMs, bitrateKbps);
		selectedJob = null;
		allowPartialDraft = false;
		operationError = null;
	}

	function returnToRange(): void {
		selectedJob = null;
		allowPartialDraft = false;
		operationError = null;
	}

	function timestampFromTime(value: string): number | null {
		if (segment === null) return null;
		const parts = value.split(':').map(Number);
		if (parts.length !== 3 || parts.some((part) => !Number.isFinite(part))) return null;
		const date = new Date(segment.start_time_ms);
		return Date.UTC(
			date.getUTCFullYear(),
			date.getUTCMonth(),
			date.getUTCDate(),
			parts[0],
			parts[1],
			parts[2]
		);
	}

	function clampToSegment(timestampMs: number): number {
		if (segment === null) return timestampMs;
		return Math.max(segment.start_time_ms, Math.min(segment.end_time_ms, timestampMs));
	}

	function changeStart(event: Event): void {
		if (range === null) return;
		const timestampMs = timestampFromTime((event.currentTarget as HTMLInputElement).value);
		if (timestampMs === null) return;
		range = updateExportRange(range, clampToSegment(timestampMs), range.endMs, bitrateKbps);
	}

	function changeEnd(event: Event): void {
		if (range === null) return;
		const timestampMs = timestampFromTime((event.currentTarget as HTMLInputElement).value);
		if (timestampMs === null) return;
		range = updateExportRange(range, range.startMs, clampToSegment(timestampMs), bitrateKbps);
	}
</script>

<section
	data-keep-export
	data-keep-export-paper-frame={paperFrame || undefined}
	data-export-start-ms={range?.startMs}
	data-export-end-ms={range?.endMs}
	data-export-status={job?.status ?? 'draft'}
	class="mx-auto overflow-hidden border bg-surface {jobPresentation && job
		? `h-[369px] rounded-lg ${
				job.status === 'partial'
					? 'border-[#E8A33D57]'
					: job.status === 'failed'
						? 'border-[#C93B4066]'
						: 'border-hairline-strong'
			}`
		: paperFrame
			? 'h-[413px] w-[467px] max-w-none rounded-lg border-hairline [font-synthesis:none]'
			: 'max-w-2xl rounded-md border-hairline'}"
	aria-label="Export a range"
>
	{#if !jobPresentation || !job}
		<header class="flex h-12 items-center gap-2 border-b border-hairline px-4">
			<DownloadIcon class="size-4 text-primary-soft" />
			<h2 class="text-sm font-semibold">Export a range</h2>
		</header>
	{/if}
	{#if range && segment}
		{#if job}
			{#if jobPresentation}
				<div
					class="flex h-[41px] translate-y-px items-center gap-2 border-b border-hairline px-[18px]"
				>
					<span
						class="size-[7px] rounded-full {job.status === 'running'
							? 'bg-primary'
							: job.status === 'ready'
								? 'bg-healthy'
								: job.status === 'partial'
									? 'bg-activity'
									: 'bg-live'}"
					></span>
					<p class="font-mono text-2xs leading-3 tracking-caps text-text-faint">
						{job.status === 'running'
							? 'A · RUNNING'
							: job.status === 'ready'
								? 'B · READY'
								: job.status === 'partial'
									? 'C · PARTIAL — THE RANGE CROSSES A GAP'
									: 'D · FAILED'}
					</p>
				</div>
			{/if}
			<div
				data-export-job={job.id}
				class="space-y-4 {jobPresentation ? 'translate-y-px px-[18px] pt-5 pb-[22px]' : 'p-4'}"
			>
				{#if job.status === 'running'}
					{#if !jobPresentation}
						<div class="flex items-center gap-2 border-b border-hairline pb-3">
							<span class="size-2 rounded-full bg-primary"></span>
							<p class="font-mono text-2xs tracking-caps text-text-faint">RUNNING</p>
						</div>
					{/if}
					<div class="flex flex-col {jobPresentation ? 'h-[42px] gap-[5px]' : 'gap-1'}">
						<h3 class="text-lg-plus font-semibold {jobPresentation ? 'leading-[22px]' : ''}">
							{sourceName} · {formatDuration(job.requestedEndMs - job.requestedStartMs)}
						</h3>
						<p class="font-mono text-xs text-text-faint {jobPresentation ? 'leading-[15px]' : ''}">
							{formatRange(job.requestedStartMs, job.requestedEndMs)}
						</p>
					</div>
					<div class="space-y-2 {jobPresentation ? 'h-[26px]' : ''}">
						<div
							class="h-1.5 overflow-hidden rounded-full bg-hairline"
							role="progressbar"
							aria-label="Export progress"
							aria-valuemin="0"
							aria-valuemax="100"
							aria-valuenow={Math.round(job.progress * 100)}
						>
							<div
								class="h-full bg-primary"
								style:width={jobPresentation ? '213px' : `${Math.round(job.progress * 100)}%`}
							></div>
						</div>
						<div
							class="flex items-center justify-between font-mono text-2xs leading-3 tracking-caps"
						>
							<p class="text-text-muted">
								{Math.round(job.progress * 100)}% · {formatJobBytes(job.bytesWritten)} OF
								{formatJobBytes(job.estimatedBytes)}
							</p>
							{#if jobPresentation}<span class="text-text-faint">~4s LEFT</span>{/if}
						</div>
					</div>
					<p
						class="rounded-sm border border-hairline bg-raised px-3.5 py-3 text-sm text-text-muted {jobPresentation
							? 'leading-[18px]'
							: 'leading-5'}"
					>
						You can leave this page. The job runs on the server and will be waiting in Keep when you
						come back.
					</p>
					<div class="flex items-center gap-2.5">
						<CapabilityGate {...capabilityActions.createExport}>
							<button
								type="button"
								class="inline-flex h-8 items-center justify-center gap-1.5 rounded-sm border border-hairline-strong bg-raised px-3.5 font-medium disabled:opacity-45 {jobPresentation
									? 'w-[71px] shrink-0 text-sm leading-4'
									: 'text-xs'}"
								disabled={submitting}
								onclick={() => void cancelJob()}
							>
								{#if !jobPresentation}<XIcon class="size-3.5" />{/if}
								{submitting ? 'Cancelling' : 'Cancel'}
							</button>
						</CapabilityGate>
						<span
							class="font-mono text-2xs leading-3 tracking-caps text-text-faint {jobPresentation
								? 'w-[223px] shrink-0 uppercase'
								: ''}"
						>
							Nothing is written until it finishes
						</span>
					</div>
				{:else if job.status === 'ready'}
					{#if !jobPresentation}
						<div class="flex items-center gap-2 border-b border-hairline pb-3">
							<span class="size-2 rounded-full bg-healthy"></span>
							<p class="font-mono text-2xs tracking-caps text-text-faint">READY</p>
						</div>
					{/if}
					<div class="flex flex-col {jobPresentation ? 'h-[42px] gap-[5px]' : 'gap-1'}">
						<h3 class="text-lg-plus font-semibold {jobPresentation ? 'leading-[22px]' : ''}">
							Your file is ready
						</h3>
						<p
							class="font-mono text-xs break-all text-text-faint {jobPresentation
								? 'leading-[15px]'
								: ''}"
						>
							{job.fileName ?? 'KeepPeek export'}
						</p>
					</div>
					<dl
						class="overflow-hidden rounded-sm border border-hairline bg-raised font-mono text-xs {jobPresentation
							? 'h-[100px]'
							: ''}"
					>
						<div
							class="flex items-center justify-between gap-3 border-b border-hairline px-3.5 {jobPresentation
								? 'py-[9px]'
								: 'py-2.5'}"
						>
							<dt class="text-2xs leading-3 tracking-caps text-text-faint">SIZE</dt>
							<dd class="leading-[14px]">
								{formatJobBytes(job.bytesWritten)} · MP4 · NO RE-ENCODE
							</dd>
						</div>
						<div
							class="flex items-center justify-between gap-3 border-b border-hairline px-3.5 {jobPresentation
								? 'py-[9px]'
								: 'py-2.5'}"
						>
							<dt class="text-2xs leading-3 tracking-caps text-text-faint">SHA-256</dt>
							<dd data-export-checksum class="min-w-0 text-right leading-[14px] break-all">
								{formatChecksum(job.sha256)}
							</dd>
						</div>
						<div
							class="flex items-center justify-between gap-3 px-3.5 {jobPresentation
								? 'py-[9px]'
								: 'py-2.5'}"
						>
							<dt class="text-2xs leading-3 tracking-caps text-text-faint">LINK EXPIRES</dt>
							<dd class="leading-[14px] text-activity {jobPresentation ? 'uppercase' : ''}">
								{formatExpiry(job.expiresAtMs)}
							</dd>
						</div>
					</dl>
					{#if job.alignedStartMs !== null && job.alignedStartMs < job.requestedStartMs}
						<p class="flex items-start gap-2 text-xs leading-5 text-text-muted">
							<InfoIcon class="mt-0.5 size-3.5 shrink-0" />
							The file starts at {formatTime(job.alignedStartMs)} to include the preceding keyframe.
						</p>
					{/if}
					<div class={jobPresentation ? 'space-y-[10px]' : 'space-y-4'}>
						<CapabilityGate {...capabilityActions.createExport} class="w-full justify-center py-2">
							<button
								type="button"
								class="inline-flex h-9 w-full items-center justify-center gap-2 rounded-sm bg-primary px-4 font-semibold text-on-primary disabled:opacity-45 {jobPresentation
									? 'text-base leading-[18px]'
									: 'text-sm'}"
								disabled={downloading}
								onclick={() => void downloadJob()}
							>
								<DownloadIcon class={jobPresentation ? 'size-3.5' : 'size-4'} />
								{downloading ? 'Verifying download' : 'Download'}
							</button>
						</CapabilityGate>
						<p
							class="text-text-muted {jobPresentation
								? 'text-sm leading-[18px]'
								: 'text-xs leading-5'}"
						>
							The file keeps the camera's own frames and the source timestamps. The checksum is what
							an insurer or a police report quotes.
						</p>
					</div>
				{:else if job.status === 'partial'}
					{#if !jobPresentation}
						<div class="flex items-center gap-2 border-b border-activity/45 pb-3">
							<span class="size-2 rounded-full bg-activity"></span>
							<p class="font-mono text-2xs tracking-caps text-text-faint">
								PARTIAL · THE RANGE CROSSES A GAP
							</p>
						</div>
					{/if}
					<div class="flex flex-col {jobPresentation ? 'h-[42px] gap-[5px]' : 'gap-1'}">
						<h3 class="text-lg-plus font-semibold {jobPresentation ? 'leading-[22px]' : ''}">
							{formatDuration(
								job.requestedEndMs - job.requestedStartMs - missingDuration(job),
								jobPresentation
							)} of {jobPresentation ? 'the ' : ''}{formatDuration(
								job.requestedEndMs - job.requestedStartMs,
								jobPresentation
							)} you asked for
						</h3>
						{#each job.missingRanges as missing (`${missing.startMs}-${missing.endMs}`)}
							<p
								data-export-missing-range
								class="font-mono text-xs text-activity {jobPresentation ? 'leading-[15px]' : ''}"
							>
								NOTHING WAS RECORDED {formatTime(missing.startMs)} → {formatTime(missing.endMs)}
							</p>
						{/each}
					</div>
					<div class="flex flex-col {jobPresentation ? 'h-[33px] gap-[7px]' : 'gap-2'}">
						<div class="flex h-3.5 overflow-hidden rounded-xs bg-hairline">
							{#each partialSections as section, index (`${section.kind}-${index}`)}
								<div
									class={section.kind === 'available'
										? 'h-full bg-availability'
										: 'h-full border-x border-dashed border-activity bg-ground'}
									style:width={`${section.percent}%`}
								></div>
							{/each}
						</div>
						<div
							class="flex justify-between font-mono text-2xs leading-3 tracking-caps text-text-faint"
						>
							<span>{formatTime(job.requestedStartMs)}</span>
							<span class="text-activity">{formatDuration(missingDuration(job))} MISSING</span>
							<span>{formatTime(job.requestedEndMs)}</span>
						</div>
					</div>
					<p
						class="rounded-sm border border-activity/45 bg-activity/5 px-3.5 py-3 text-sm text-text-muted {jobPresentation
							? 'leading-[18px]'
							: 'leading-5'}"
					>
						The export will contain both sides of the gap as one file with a real time break in it,
						not a silent join.
					</p>
					<div class="flex flex-wrap items-center gap-2.5">
						<CapabilityGate {...capabilityActions.createExport}>
							<button
								type="button"
								class="inline-flex h-8 items-center justify-center rounded-sm bg-primary px-3.5 font-semibold whitespace-nowrap text-on-primary disabled:opacity-45 {jobPresentation
									? 'w-[138px] text-sm leading-4'
									: 'text-xs'}"
								disabled={submitting}
								onclick={() => void createJob(true)}
							>
								{submitting ? 'Starting export' : 'Export what exists'}
							</button>
						</CapabilityGate>
						{#if trimTarget}
							<button
								type="button"
								class="inline-flex h-8 items-center justify-center rounded-sm border border-hairline-strong bg-raised px-3.5 font-medium whitespace-nowrap {jobPresentation
									? 'w-[124px] text-sm leading-4'
									: 'text-xs'}"
								onclick={() => trimRange(trimTarget)}
							>
								Trim to {trimTarget.label}
							</button>
						{/if}
					</div>
				{:else if job.status === 'failed'}
					{#if !jobPresentation}
						<div class="flex items-center gap-2 border-b border-live/40 pb-3">
							<span class="size-2 rounded-full bg-live"></span>
							<p class="font-mono text-2xs tracking-caps text-text-faint">FAILED</p>
						</div>
					{/if}
					<div class="flex flex-col {jobPresentation ? 'h-[42px] gap-[5px]' : 'gap-1'}">
						<h3 class="text-lg-plus font-semibold {jobPresentation ? 'leading-[22px]' : ''}">
							{job.error?.toLowerCase().includes('space left')
								? 'The disk filled while writing'
								: job.burnInTimestamp
									? 'Timestamp burn-in is unavailable'
									: 'The export did not finish'}
						</h3>
						<p class="font-mono text-xs text-live-text {jobPresentation ? 'leading-[15px]' : ''}">
							STOPPED AT {Math.round(job.progress * 100)}% · NOTHING PARTIAL WAS OFFERED
						</p>
					</div>
					<div
						class="rounded-sm border border-hairline bg-raised px-3.5 py-3 {jobPresentation
							? 'h-[78px]'
							: ''}"
					>
						<p class="font-mono text-2xs leading-3 tracking-caps text-text-faint">
							WHAT THE SERVER SAID
						</p>
						<p class="mt-2 font-mono text-xs leading-4 break-words">
							{job.error ?? 'No failure reason was reported.'}
						</p>
					</div>
					<p class="text-sm text-text-muted {jobPresentation ? 'leading-[18px]' : 'leading-5'}">
						Your range is still selected and still valid. Resolve the server problem, then retry
						without finding the moment again.
					</p>
					<div class="flex flex-wrap items-center gap-2.5">
						{#if job.retryable}
							<CapabilityGate {...capabilityActions.createExport}>
								<button
									type="button"
									class="inline-flex h-8 items-center justify-center gap-1.5 rounded-sm bg-primary px-3.5 font-semibold text-on-primary disabled:opacity-45 {jobPresentation
										? 'w-[61px] text-sm leading-4'
										: 'text-xs'}"
									disabled={submitting}
									onclick={() => void retryJob()}
								>
									{#if !jobPresentation}<RotateCcwIcon class="size-3.5" />{/if}
									{submitting ? 'Retrying' : 'Retry'}
								</button>
							</CapabilityGate>
						{/if}
						<a
							href={`${resolve('/settings')}#storage`}
							class="inline-flex h-8 items-center justify-center gap-1.5 rounded-sm border border-hairline-strong bg-raised px-3.5 font-medium whitespace-nowrap {jobPresentation
								? 'w-[109px] text-sm leading-4'
								: 'text-xs'}"
						>
							{#if !jobPresentation}<HardDriveIcon class="size-3.5" />{/if}
							Open storage
						</a>
					</div>
					<p
						class="flex items-center gap-2 font-mono text-2xs leading-3 tracking-caps text-text-faint {jobPresentation
							? 'h-[14px] pt-0.5 uppercase'
							: ''}"
					>
						<span class="size-1.5 rounded-full bg-healthy"></span>
						Recording never paused for this job
					</p>
				{:else}
					<div class="grid min-h-48 place-items-center text-center">
						<div class="max-w-sm space-y-3">
							{#if job.status === 'cancelled'}
								<XIcon class="mx-auto size-6 text-text-faint" />
								<h3 class="text-lg font-semibold">Export cancelled</h3>
								<p class="text-sm text-text-muted">No partial file was kept.</p>
							{:else}
								<TriangleAlertIcon class="mx-auto size-6 text-activity" />
								<h3 class="text-lg font-semibold">The download expired</h3>
								<p class="text-sm text-text-muted">The selected range is still available.</p>
							{/if}
							<button
								type="button"
								class="inline-flex h-8 items-center rounded-sm border border-hairline-strong bg-raised px-3.5 text-xs font-medium"
								onclick={returnToRange}
							>
								Return to range
							</button>
						</div>
					</div>
				{/if}
				{#if operationError}
					<div
						class="flex items-start gap-2 rounded-sm border border-live/40 bg-live/5 px-3 py-2.5 text-xs text-live-text"
						role="alert"
					>
						<TriangleAlertIcon class="mt-0.5 size-3.5 shrink-0" />
						<p>{operationError}</p>
					</div>
				{/if}
			</div>
		{:else}
			<div class={paperFrame ? 'flex h-[363px] flex-col gap-3.5 p-4' : 'space-y-4 p-4'}>
				<p class="text-xs leading-5 text-text-muted">
					The current recording segment defines the available draft range. Keyframe positions are
					not reported by this server.
				</p>
				<div class="grid grid-cols-2 gap-2.5">
					<label
						class="grid font-mono text-2xs tracking-caps text-text-faint {paperFrame
							? 'gap-[5px]'
							: 'gap-1.5'}"
					>
						FROM
						<input
							type={paperFrame ? 'text' : 'time'}
							step={paperFrame ? undefined : 1}
							value={formatTimeInput(range.startMs)}
							readonly={paperFrame}
							class="rounded-sm border border-hairline-strong bg-raised px-3 font-mono text-xs tracking-normal text-foreground outline-none focus:border-ring focus:ring-1 focus:ring-ring {paperFrame
								? 'h-[34px]'
								: 'h-10'}"
							onchange={changeStart}
						/>
					</label>
					<label
						class="grid font-mono text-2xs tracking-caps text-text-faint {paperFrame
							? 'gap-[5px]'
							: 'gap-1.5'}"
					>
						TO
						<input
							type={paperFrame ? 'text' : 'time'}
							step={paperFrame ? undefined : 1}
							value={formatTimeInput(range.endMs)}
							readonly={paperFrame}
							class="rounded-sm border border-hairline-strong bg-raised px-3 font-mono text-xs tracking-normal text-foreground outline-none focus:border-ring focus:ring-1 focus:ring-ring {paperFrame
								? 'h-[34px]'
								: 'h-10'}"
							onchange={changeEnd}
						/>
					</label>
				</div>

				<dl class="space-y-2 rounded-md bg-raised p-3 text-xs">
					<div class="flex items-center justify-between gap-3">
						<dt class="text-text-muted">Duration</dt>
						<dd class="font-mono">{formatDuration(range.durationMs)}</dd>
					</div>
					<div class="flex items-center justify-between gap-3">
						<dt class="text-text-muted">Estimated size</dt>
						<dd class="font-mono">{formatBytes(range.estimatedBytes)}</dd>
					</div>
					<div class="flex items-center justify-between gap-3">
						<dt class="text-text-muted">Container</dt>
						<dd class="font-mono">MP4 · no re-encode</dd>
					</div>
				</dl>

				<label
					class="flex items-center gap-2.5 text-xs text-text-muted {paperFrame
						? 'h-[18px]'
						: 'min-h-8'}"
				>
					<input type="checkbox" class="size-4 accent-primary" bind:checked={burnInTimestamp} />
					Burn in timestamp (forces re-encode)
				</label>

				{#if candidateMatch.exactReady || candidateMatch.related.length > 0}
					<div
						data-export-duplicate
						class="space-y-3 rounded-sm border border-activity/45 bg-activity/5 px-3.5 py-3"
					>
						<div class="flex items-start gap-2.5">
							<LayersIcon class="mt-0.5 size-4 shrink-0 text-activity" />
							<div class="min-w-0 space-y-1">
								<p class="text-sm font-semibold">
									{candidateMatch.exactReady
										? 'A matching export is already ready'
										: 'Previous exports overlap this range'}
								</p>
								<p class="text-xs leading-5 text-text-muted">
									{candidateMatch.exactReady
										? 'Reuse the verified artifact or deliberately create a fresh copy.'
										: 'Review the related evidence before starting another export.'}
								</p>
							</div>
						</div>
						{#if candidateMatch.related.length > 0}
							<ul class="divide-y divide-hairline border-y border-hairline font-mono text-xs">
								{#each candidateMatch.related.slice(0, 3) as candidate (candidate.id)}
									<li class="flex items-center justify-between gap-3 py-2">
										<span class="min-w-0 truncate">
											{formatRange(candidate.requestedStartMs, candidate.requestedEndMs)}
										</span>
										<span class="shrink-0 text-text-faint uppercase">{candidate.status}</span>
									</li>
								{/each}
							</ul>
						{/if}
						<div class="flex flex-wrap gap-2">
							{#if candidateMatch.exactReady}
								<button
									type="button"
									class="inline-flex h-9 items-center gap-2 rounded-sm bg-primary px-3.5 text-xs font-semibold text-on-primary"
									onclick={() => rememberJob(candidateMatch.exactReady!)}
								>
									<DownloadIcon class="size-3.5" /> Use existing export
								</button>
							{/if}
							<CapabilityGate {...capabilityActions.createExport}>
								<button
									type="button"
									class="inline-flex h-9 items-center rounded-sm border border-hairline-strong bg-raised px-3.5 text-xs font-semibold disabled:opacity-45"
									disabled={submitting}
									onclick={() => void createJob(allowPartialDraft, true)}
								>
									{submitting ? 'Creating export' : 'Create fresh export'}
								</button>
							</CapabilityGate>
						</div>
					</div>
				{:else}
					<CapabilityGate {...capabilityActions.createExport} class="w-full justify-center py-2">
						<button
							type="button"
							class="inline-flex h-9 w-full items-center justify-center gap-2 rounded-sm bg-primary px-4 text-sm font-semibold text-on-primary disabled:opacity-45"
							disabled={submitting}
							onclick={() => void createJob()}
						>
							<DownloadIcon class="size-4" />
							{submitting ? 'Creating export' : 'Create export'}
						</button>
					</CapabilityGate>
				{/if}
				{#if operationError}
					<div
						class="flex items-start gap-2 rounded-sm border border-live/40 bg-live/5 px-3 py-2.5 text-xs text-live-text"
						role="alert"
					>
						<TriangleAlertIcon class="mt-0.5 size-3.5 shrink-0" />
						<p>{operationError}</p>
					</div>
				{/if}
				<div class="flex items-start gap-2 text-xs leading-5 text-text-faint">
					<InfoIcon class="mt-0.5 size-3.5 shrink-0" />
					<p>
						Range and options remain editable. Job creation requires the advertised media-export
						capability.
					</p>
				</div>
			</div>
		{/if}
	{:else}
		<div class="grid min-h-56 place-items-center p-5 text-center">
			<div class="space-y-2">
				<CheckCircleIcon class="mx-auto size-5 text-text-faint" />
				<p class="text-sm font-medium">No recording segment selected.</p>
				<p class="text-xs text-text-muted">
					Choose footage in Timeline before preparing an export.
				</p>
			</div>
		</div>
	{/if}
</section>
