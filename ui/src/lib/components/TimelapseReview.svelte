<script lang="ts">
	import { untrack } from 'svelte';
	import { useCapabilityState } from '$lib/capability-context';
	import { useControlClient } from '$lib/control-context';
	import type { MediaExportJob } from '$lib/control-client';
	import { saveExportDownload } from '$lib/save-export-download';
	import {
		MAX_CONCURRENT_THUMBNAIL_FETCHES,
		MAX_THUMBNAIL_CACHE_BYTES,
		TimelapseThumbnailCache,
		TimelapseThumbnailQueue,
		decodeTimelapseWindowToBlob,
		formatBytesCompact,
		formatDurationCompact,
		resolveTimelapsePresetRange,
		timelapseGapLabel,
		type TimelapsePlanEntry,
		type TimelapsePlanResponse,
		type TimelapsePlanSample,
		type TimelapsePreset
	} from '$lib/timelapse-review';
	import { Button } from '$lib/components/ui/button/index.js';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import FileJsonIcon from '@lucide/svelte/icons/file-json';
	import FilmIcon from '@lucide/svelte/icons/film';
	import PauseIcon from '@lucide/svelte/icons/pause';
	import PlayIcon from '@lucide/svelte/icons/play';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import XIcon from '@lucide/svelte/icons/x';

	type Props = {
		sourceId: string;
		sourceName: string;
		streamId: 'main' | 'sub';
		anchorMs?: number | null;
		onseek: (timestampMs: number) => void;
	};

	let { sourceId, sourceName, streamId, anchorMs = null, onseek }: Props = $props();

	const controlClient = useControlClient();
	const capabilities = useCapabilityState();

	let preset = $state<TimelapsePreset>('last_hour');
	let customStartInput = $state('');
	let customEndInput = $state('');
	let cadenceMode = $state<'count' | 'interval'>('count');
	let targetSampleCount = $state(24);
	let intervalSeconds = $state(120);
	let outputFps = $state(10);
	let outputResolution = $state<'640x360' | '1280x720' | '1920x1080'>('1280x720');
	let timestampOverlay = $state(true);
	let allowPartial = $state(false);
	let confirmedExpensive = $state(false);

	let plan = $state.raw<TimelapsePlanResponse | null>(null);
	let plannedSignature = $state<string | null>(null);
	let items = $state.raw<TimelapsePlanEntry[]>([]);
	let nextPageToken = $state('');
	let loadingPlan = $state(false);
	let loadingMore = $state(false);
	let planError = $state<string | null>(null);

	let thumbUrls = $state.raw<Record<string, string>>({});
	let thumbErrors = $state.raw<Record<string, string>>({});

	let autoplay = $state(false);
	let activeSampleIndex = $state(0);

	let timelapseJobs = $state.raw<MediaExportJob[]>([]);
	let activeJob = $state.raw<MediaExportJob | null>(null);
	let submittingExport = $state(false);
	let downloadingId = $state<string | null>(null);
	let exportError = $state<string | null>(null);

	let planRequestSerial = 0;

	let timelapseSupported = $derived(capabilities.supports('keeppeek.timelapse.v1'));
	let exportCapabilitySupported = $derived(capabilities.supports('keeppeek.timelapse-export.v1'));
	let sampleItems = $derived(
		items.filter((entry): entry is TimelapsePlanSample => entry.kind === 'sample')
	);
	let activeSample = $derived(sampleItems[activeSampleIndex] ?? null);
	let dimensions = $derived.by(() => {
		const [w, h] = outputResolution.split('x').map(Number);
		return { maxWidth: w || 1280, maxHeight: h || 720 };
	});
	let currentDraftSignature = $derived(
		JSON.stringify({
			sourceId,
			streamId,
			preset,
			customStartInput: preset === 'custom' ? customStartInput : '',
			customEndInput: preset === 'custom' ? customEndInput : '',
			cadenceMode,
			targetSampleCount: Math.max(1, Math.min(3600, Math.round(targetSampleCount))),
			intervalSeconds: Math.max(1, Math.round(intervalSeconds)),
			outputFps,
			outputResolution,
			timestampOverlay
		})
	);
	let previewStale = $derived(plan !== null && plannedSignature !== currentDraftSignature);
	let canGenerate = $derived(
		Boolean(
			plan &&
			!previewStale &&
			plan.estimate.sampleCount > 0 &&
			plan.estimate.encoderAvailable &&
			exportCapabilitySupported &&
			(!plan.estimate.gapCount || allowPartial) &&
			(!plan.estimate.requiresConfirmation || confirmedExpensive) &&
			!submittingExport
		)
	);

	const cache = new TimelapseThumbnailCache(MAX_THUMBNAIL_CACHE_BYTES);
	const queue = new TimelapseThumbnailQueue(
		cache,
		async (sample, signal) => {
			const win = await controlClient.fetchTimelapseSampleWindow({
				sourceId,
				streamId,
				sampleToken: sample.sampleToken,
				signal
			});
			const blob = await decodeTimelapseWindowToBlob(win, signal);
			return { url: URL.createObjectURL(blob), bytes: blob.size };
		},
		(key, update) => {
			if (update.url) {
				thumbUrls = { ...thumbUrls, [key]: update.url };
			}
			if (update.error) {
				thumbErrors = { ...thumbErrors, [key]: update.error };
			}
		},
		MAX_CONCURRENT_THUMBNAIL_FETCHES
	);

	function toLocalInputValue(ms: number): string {
		const date = new Date(ms);
		const pad = (n: number) => String(n).padStart(2, '0');
		return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
	}

	function formatUtc(ms: number): string {
		return new Date(ms).toISOString().replace('T', ' ').replace('.000Z', ' UTC');
	}

	function formatLocal(ms: number): string {
		return new Date(ms).toLocaleTimeString([], {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	function currentCadence():
		{ mode: 'interval'; intervalMs: number } | { mode: 'count'; targetSampleCount: number } {
		if (cadenceMode === 'interval') {
			return { mode: 'interval', intervalMs: Math.max(1, Math.round(intervalSeconds)) * 1000 };
		}
		return {
			mode: 'count',
			targetSampleCount: Math.max(1, Math.min(3600, Math.round(targetSampleCount)))
		};
	}

	function currentRange(): { startMs: number; endMs: number } {
		const nowMs = anchorMs ?? Date.now();
		if (preset === 'custom') {
			const startMs = Date.parse(customStartInput);
			const endMs = Date.parse(customEndInput);
			return resolveTimelapsePresetRange('custom', nowMs, { startMs, endMs });
		}
		return resolveTimelapsePresetRange(preset, nowMs);
	}

	async function loadPlan(append = false): Promise<void> {
		if (!sourceId || !timelapseSupported) return;
		const serial = ++planRequestSerial;
		const requestSignature = currentDraftSignature;
		planError = null;
		if (append) {
			loadingMore = true;
		} else {
			autoplay = false;
			activeSampleIndex = 0;
			loadingPlan = true;
			queue.reset();
			thumbUrls = {};
			thumbErrors = {};
			confirmedExpensive = false;
		}
		try {
			const range = currentRange();
			const response = await controlClient.planTimelapse({
				sourceId,
				streamId,
				startMs: range.startMs,
				endMs: range.endMs,
				cadence: currentCadence(),
				pageSize: 24,
				pageToken: append ? nextPageToken : '',
				outputPreview: {
					outputFps,
					maxWidth: dimensions.maxWidth,
					maxHeight: dimensions.maxHeight,
					timestampOverlay
				}
			});
			if (serial !== planRequestSerial) return;
			plan = response;
			plannedSignature = requestSignature;
			items = append ? [...items, ...response.items] : response.items;
			nextPageToken = response.nextPageToken;
		} catch (cause) {
			if (serial !== planRequestSerial) return;
			planError = cause instanceof Error ? cause.message : 'Unable to plan timelapse review.';
		} finally {
			if (serial === planRequestSerial) {
				loadingPlan = false;
				loadingMore = false;
			}
		}
	}

	async function refreshHistory(): Promise<void> {
		const expectedSource = sourceId;
		try {
			const jobs = await controlClient.listExports();
			if (expectedSource !== sourceId) return;
			timelapseJobs = jobs.filter((job) => job.timelapse && job.sourceId === expectedSource);
			if (activeJob && activeJob.sourceId === expectedSource) {
				const updated = timelapseJobs.find((job) => job.id === activeJob?.id);
				activeJob = updated ?? timelapseJobs.find((job) => job.status === 'running') ?? null;
			} else {
				activeJob = timelapseJobs.find((job) => job.status === 'running') ?? null;
			}
		} catch {
			// History load errors do not block interactive sparse review.
		}
	}

	async function startTimelapseExport(): Promise<void> {
		if (!plan || !canGenerate) return;
		submittingExport = true;
		exportError = null;
		try {
			const created = await controlClient.createExport({
				sourceId,
				streamId,
				startMs: plan.requestedStartMs,
				endMs: plan.requestedEndMs,
				allowPartial,
				burnInTimestamp: false,
				timelapse: {
					cadence: currentCadence(),
					outputFps,
					maxWidth: dimensions.maxWidth,
					maxHeight: dimensions.maxHeight,
					timestampOverlay,
					confirmedExpensive,
					expectedSourceFingerprint: plan.sourceFingerprint
				}
			});
			activeJob = created;
			await refreshHistory();
		} catch (cause) {
			exportError = cause instanceof Error ? cause.message : 'Unable to start timelapse export.';
		} finally {
			submittingExport = false;
		}
	}

	async function cancelJob(jobId: string): Promise<void> {
		exportError = null;
		try {
			activeJob = await controlClient.cancelExport(jobId);
			await refreshHistory();
		} catch (cause) {
			exportError = cause instanceof Error ? cause.message : 'Unable to cancel timelapse export.';
		}
	}

	async function retryJob(jobId: string): Promise<void> {
		exportError = null;
		try {
			activeJob = await controlClient.retryExport(jobId);
			await refreshHistory();
		} catch (cause) {
			exportError = cause instanceof Error ? cause.message : 'Unable to retry timelapse export.';
		}
	}

	async function downloadArtifact(jobId: string, artifact: 'media' | 'manifest'): Promise<void> {
		downloadingId = `${jobId}:${artifact}`;
		exportError = null;
		try {
			const downloaded = await controlClient.downloadExport(jobId, artifact);
			const fallbackName =
				artifact === 'manifest' ? `${jobId}.manifest.json` : `${jobId}-timelapse.mp4`;
			saveExportDownload({ ...downloaded, fileName: downloaded.fileName ?? fallbackName });
		} catch (cause) {
			exportError =
				cause instanceof Error ? cause.message : `Unable to download timelapse ${artifact}.`;
		} finally {
			downloadingId = null;
		}
	}

	function selectPreset(next: TimelapsePreset): void {
		preset = next;
		const nowMs = anchorMs ?? Date.now();
		if (next === 'custom' && (!customStartInput || !customEndInput)) {
			customStartInput = toLocalInputValue(nowMs - 60 * 60_000);
			customEndInput = toLocalInputValue(nowMs);
		}
		if (next !== 'custom') {
			void loadPlan(false);
		}
	}

	function observeSample(node: HTMLElement, sample: TimelapsePlanSample) {
		if (typeof IntersectionObserver === 'undefined') {
			queue.request(sample);
			return {};
		}
		const observer = new IntersectionObserver(
			(entries) => {
				if (entries.some((entry) => entry.isIntersecting)) {
					queue.request(sample);
					observer.disconnect();
				}
			},
			{ rootMargin: '120px' }
		);
		observer.observe(node);
		return {
			destroy() {
				observer.disconnect();
			}
		};
	}

	$effect(() => {
		const key = `${sourceId}:${streamId}`;
		if (!key) return;
		untrack(() => {
			const nowMs = anchorMs ?? Date.now();
			plan = null;
			plannedSignature = null;
			items = [];
			nextPageToken = '';
			planError = null;
			activeJob = null;
			timelapseJobs = [];
			exportError = null;
			allowPartial = false;
			confirmedExpensive = false;
			autoplay = false;
			activeSampleIndex = 0;
			thumbUrls = {};
			thumbErrors = {};
			customStartInput = toLocalInputValue(nowMs - 60 * 60_000);
			customEndInput = toLocalInputValue(nowMs);
			void loadPlan(false);
			void refreshHistory();
		});
		return () => {
			planRequestSerial += 1;
			queue.reset();
		};
	});

	$effect(() => {
		if (previewStale) {
			confirmedExpensive = false;
		}
	});

	$effect(() => {
		if (!autoplay || sampleItems.length <= 1) return;
		const handleVisibility = () => {
			if (document.hidden) autoplay = false;
		};
		document.addEventListener('visibilitychange', handleVisibility);
		const timer = setInterval(() => {
			if (document.hidden) {
				autoplay = false;
				return;
			}
			activeSampleIndex = (activeSampleIndex + 1) % sampleItems.length;
			const next = sampleItems[activeSampleIndex];
			if (next) queue.request(next);
		}, 700);
		return () => {
			clearInterval(timer);
			document.removeEventListener('visibilitychange', handleVisibility);
		};
	});

	$effect(() => {
		const job = activeJob;
		if (!job || job.status !== 'running') return;
		const timer = setInterval(() => {
			void controlClient
				.getExport(job.id)
				.then((updated) => {
					activeJob = updated;
					if (updated.status !== 'running') void refreshHistory();
				})
				.catch(() => {});
		}, 1200);
		return () => clearInterval(timer);
	});
</script>

<section
	data-timelapse-review
	aria-label="Timelapse review and export"
	class="grid gap-4 pb-6 lg:grid-cols-[minmax(0,1fr)_22rem]"
>
	<div class="space-y-4">
		<div class="rounded-md border border-hairline bg-surface p-4">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<div>
					<h2 class="text-base font-semibold">Interactive Timelapse Review</h2>
					<p class="text-xs text-text-muted">
						Sparse sampled frames for {sourceName} ({streamId}). Clicking any frame seeks Keep to
						its exact recorded timestamp.
					</p>
				</div>
				<div class="flex flex-wrap items-center gap-2">
					<Button
						data-timelapse-preset="last_15m"
						variant={preset === 'last_15m' ? 'default' : 'outline'}
						class="h-11 md:h-8"
						onclick={() => selectPreset('last_15m')}
					>
						Last 15 min
					</Button>
					<Button
						data-timelapse-preset="last_hour"
						variant={preset === 'last_hour' ? 'default' : 'outline'}
						class="h-11 md:h-8"
						onclick={() => selectPreset('last_hour')}
					>
						Last hour
					</Button>
					<Button
						data-timelapse-preset="custom"
						variant={preset === 'custom' ? 'default' : 'outline'}
						class="h-11 md:h-8"
						onclick={() => selectPreset('custom')}
					>
						Custom
					</Button>
				</div>
			</div>

			<div class="mt-3 grid gap-3 lg:grid-cols-4 sm:grid-cols-2">
				{#if preset === 'custom'}
					<label class="grid gap-1 text-xs">
						<span class="text-text-muted">Start (local)</span>
						<input
							data-timelapse-custom-start
							type="datetime-local"
							bind:value={customStartInput}
							class="h-11 rounded-sm border border-hairline bg-background px-2 text-xs md:h-9"
						/>
					</label>
					<label class="grid gap-1 text-xs">
						<span class="text-text-muted">End (local)</span>
						<input
							data-timelapse-custom-end
							type="datetime-local"
							bind:value={customEndInput}
							class="h-11 rounded-sm border border-hairline bg-background px-2 text-xs md:h-9"
						/>
					</label>
				{/if}
				<label class="grid gap-1 text-xs">
					<span class="text-text-muted">Sampling mode</span>
					<select
						data-timelapse-cadence-mode
						bind:value={cadenceMode}
						class="h-11 rounded-sm border border-hairline bg-background px-2 text-xs md:h-9"
					>
						<option value="count">Target sample count</option>
						<option value="interval">Fixed interval (seconds)</option>
					</select>
				</label>
				{#if cadenceMode === 'count'}
					<label class="grid gap-1 text-xs">
						<span class="text-text-muted">Target samples (1–3600)</span>
						<input
							data-timelapse-sample-count
							type="number"
							min="1"
							max="3600"
							bind:value={targetSampleCount}
							class="h-11 rounded-sm border border-hairline bg-background px-2 text-xs md:h-9"
						/>
					</label>
				{:else}
					<label class="grid gap-1 text-xs">
						<span class="text-text-muted">Interval (seconds)</span>
						<input
							data-timelapse-interval-secs
							type="number"
							min="1"
							bind:value={intervalSeconds}
							class="h-11 rounded-sm border border-hairline bg-background px-2 text-xs md:h-9"
						/>
					</label>
				{/if}
				<div class="flex items-end gap-2">
					<Button
						data-timelapse-plan-refresh
						variant="outline"
						class="h-11 flex-1 md:h-9"
						disabled={loadingPlan || !timelapseSupported}
						onclick={() => void loadPlan(false)}
					>
						<RefreshCwIcon class="mr-1.5 size-3.5 {loadingPlan ? 'animate-spin' : ''}" />
						Preview plan
					</Button>
					<Button
						data-timelapse-autoplay
						variant={autoplay ? 'default' : 'outline'}
						class="h-11 md:h-9"
						disabled={sampleItems.length <= 1}
						aria-pressed={autoplay}
						onclick={() => (autoplay = !autoplay)}
					>
						{#if autoplay}
							<PauseIcon class="mr-1 size-3.5" />
							Pause
						{:else}
							<PlayIcon class="mr-1 size-3.5" />
							Play
						{/if}
					</Button>
				</div>
			</div>

			{#if !timelapseSupported}
				<p role="status" class="mt-3 text-xs text-text-muted">
					Server update required for sparse timelapse review (`keeppeek.timelapse.v1`).
				</p>
			{/if}
			{#if planError}
				<div
					role="alert"
					class="mt-3 rounded-sm border border-destructive/60 bg-destructive/10 px-3 py-2 text-xs text-destructive"
				>
					{planError}
				</div>
			{/if}
		</div>

		{#if activeSample && autoplay}
			<div
				data-timelapse-autoplay-stage
				class="rounded-md border border-hairline bg-surface p-3"
				aria-live="polite"
			>
				<div class="mb-2 flex items-center justify-between text-xs">
					<span class="font-medium">
						Frame {activeSampleIndex + 1} / {sampleItems.length} · {formatLocal(
							activeSample.actualTimeMs
						)}
					</span>
					<Button size="sm" variant="outline" onclick={() => onseek(activeSample.actualTimeMs)}>
						Open in Keep
					</Button>
				</div>
				{#if thumbUrls[activeSample.tokenKey]}
					<img
						src={thumbUrls[activeSample.tokenKey]}
						alt={`Sampled frame at ${formatUtc(activeSample.actualTimeMs)}`}
						class="aspect-video w-full rounded-sm bg-black object-contain"
					/>
				{:else}
					<div
						class="flex aspect-video items-center justify-center rounded-sm bg-raised text-xs text-text-muted"
					>
						Loading preview frame…
					</div>
				{/if}
			</div>
		{/if}

		<div
			data-timelapse-grid
			class="grid grid-cols-1 gap-3 xl:grid-cols-3 sm:grid-cols-2"
			aria-live="polite"
		>
			{#each items as entry, idx (entry.kind === 'sample' ? entry.tokenKey : `gap-${idx}-${entry.startMs}`)}
				{#if entry.kind === 'sample'}
					<button
						type="button"
						data-timelapse-sample-card
						data-sample-time-ms={entry.actualTimeMs}
						use:observeSample={entry}
						class="group flex flex-col overflow-hidden rounded-md border border-hairline bg-surface text-left transition hover:border-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						onclick={() => onseek(entry.actualTimeMs)}
					>
						<div class="relative aspect-video w-full bg-raised">
							{#if thumbUrls[entry.tokenKey]}
								<img
									src={thumbUrls[entry.tokenKey]}
									alt={`Recorded frame at ${formatUtc(entry.actualTimeMs)}`}
									class="h-full w-full object-cover"
								/>
							{:else if thumbErrors[entry.tokenKey]}
								<div
									class="flex h-full flex-col items-center justify-center gap-1 p-2 text-center text-2xs text-text-muted"
								>
									<TriangleAlertIcon class="text-warning size-4" />
									<span>{thumbErrors[entry.tokenKey]}</span>
								</div>
							{:else}
								<div class="flex h-full items-center justify-center text-2xs text-text-muted">
									Loading frame…
								</div>
							{/if}
						</div>
						<div class="flex items-center justify-between gap-2 px-2.5 py-2 text-xs">
							<span class="font-mono font-medium" title={formatUtc(entry.actualTimeMs)}>
								{formatLocal(entry.actualTimeMs)}
							</span>
							<span class="text-2xs text-text-muted">
								{entry.codec} · {entry.width}×{entry.height}
							</span>
						</div>
					</button>
				{:else}
					<div
						data-timelapse-gap-card
						data-gap-reason={entry.reason}
						class="flex flex-col justify-between rounded-md border border-dashed border-hairline-strong bg-raised/60 p-3 text-xs"
					>
						<div class="text-warning flex items-center gap-1.5 font-medium">
							<TriangleAlertIcon class="size-4 shrink-0" />
							<span>{timelapseGapLabel(entry.reason)}</span>
						</div>
						<p class="mt-2 font-mono text-2xs text-text-muted">
							{formatLocal(entry.startMs)} – {formatLocal(entry.endMs)}
						</p>
						<p class="mt-1 text-2xs text-text-muted">
							Omitted interval ({formatDurationCompact(Math.max(0, entry.endMs - entry.startMs))});
							never filled with stale frames.
						</p>
					</div>
				{/if}
			{/each}
		</div>

		{#if nextPageToken}
			<div class="flex justify-center">
				<Button
					data-timelapse-load-more
					variant="outline"
					class="h-11 md:h-9"
					disabled={loadingMore}
					onclick={() => void loadPlan(true)}
				>
					{loadingMore ? 'Loading more…' : 'Load more sampled frames'}
				</Button>
			</div>
		{/if}
	</div>

	<aside
		data-timelapse-export-panel
		aria-label="Silent timelapse MP4 generation"
		class="space-y-4 rounded-md border border-hairline bg-surface p-4"
	>
		<div class="flex items-center gap-2">
			<FilmIcon class="size-4 text-primary" />
			<h2 class="text-sm font-semibold">Generate Silent MP4</h2>
		</div>
		<p class="text-xs text-text-muted">
			Creates a time-compressed silent MP4 plus a SHA-256 provenance manifest. Source recordings
			remain immutable.
		</p>

		{#if plan}
			<dl
				data-timelapse-preview-summary
				class="grid grid-cols-2 gap-2 rounded-sm bg-raised p-3 text-xs"
			>
				<div>
					<dt class="text-2xs text-text-muted">Actual range</dt>
					<dd class="font-mono text-2xs">
						{formatUtc(plan.requestedStartMs)} – {formatUtc(plan.requestedEndMs)}
					</dd>
				</div>
				<div>
					<dt class="text-2xs text-text-muted">Sample count</dt>
					<dd data-timelapse-estimate-samples class="font-mono font-semibold">
						{plan.estimate.sampleCount}
					</dd>
				</div>
				<div>
					<dt class="text-2xs text-text-muted">Output duration</dt>
					<dd data-timelapse-estimate-duration class="font-mono">
						{formatDurationCompact(plan.estimate.outputDurationMs)}
					</dd>
				</div>
				<div>
					<dt class="text-2xs text-text-muted">Estimated size</dt>
					<dd data-timelapse-estimate-bytes class="font-mono">
						{formatBytesCompact(plan.estimate.estimatedOutputBytes)}
					</dd>
				</div>
				<div>
					<dt class="text-2xs text-text-muted">Omitted gaps</dt>
					<dd data-timelapse-estimate-gaps class="font-mono">
						{plan.estimate.gapCount} ({formatDurationCompact(plan.estimate.gapDurationMs)})
					</dd>
				</div>
				<div>
					<dt class="text-2xs text-text-muted">Audio & Timezone</dt>
					<dd class="font-mono text-2xs">Silent · {plan.estimate.siteTimezone}</dd>
				</div>
			</dl>
		{/if}

		<div class="grid gap-3">
			<label class="grid gap-1 text-xs">
				<span class="text-text-muted">Output frame rate (1–30 FPS)</span>
				<input
					data-timelapse-fps
					type="number"
					min="1"
					max="30"
					bind:value={outputFps}
					onchange={() => void loadPlan(false)}
					class="h-11 rounded-sm border border-hairline bg-background px-2 text-xs md:h-9"
				/>
			</label>
			<label class="grid gap-1 text-xs">
				<span class="text-text-muted">Max dimensions</span>
				<select
					data-timelapse-dimensions
					bind:value={outputResolution}
					onchange={() => void loadPlan(false)}
					class="h-11 rounded-sm border border-hairline bg-background px-2 text-xs md:h-9"
				>
					<option value="640x360">640 × 360</option>
					<option value="1280x720">1280 × 720</option>
					<option value="1920x1080">1920 × 1080</option>
				</select>
			</label>
			<label class="flex min-h-11 items-center gap-2 text-xs md:min-h-8">
				<input data-timelapse-overlay type="checkbox" bind:checked={timestampOverlay} />
				<span>Overlay site-timezone timestamp (UTC provenance kept in manifest)</span>
			</label>
			{#if previewStale}
				<p
					data-timelapse-stale-preview
					role="status"
					class="text-warning rounded-sm border border-dashed border-hairline-strong bg-raised/60 px-2.5 py-2 text-xs"
				>
					Settings changed since the last preview. Run Preview plan to refresh estimates and
					confirmation before generating.
				</p>
			{/if}
			{#if plan && plan.estimate.gapCount > 0}
				<label class="text-warning flex min-h-11 items-center gap-2 text-xs md:min-h-8">
					<input data-timelapse-allow-partial type="checkbox" bind:checked={allowPartial} />
					<span>Allow partial output omitting {plan.estimate.gapCount} unavailable interval(s)</span
					>
				</label>
			{/if}
			{#if plan && plan.estimate.requiresConfirmation}
				<label class="text-warning flex min-h-11 items-center gap-2 text-xs md:min-h-8">
					<input
						data-timelapse-confirm-expensive
						type="checkbox"
						bind:checked={confirmedExpensive}
					/>
					<span>Confirm expensive generation ({plan.estimate.sampleCount} samples)</span>
				</label>
			{/if}
		</div>

		{#if !exportCapabilitySupported || (plan && !plan.estimate.encoderAvailable)}
			<p role="status" class="rounded-sm bg-raised p-2 text-xs text-text-muted">
				Local silent MP4 encoder worker is unavailable on this recorder. Interactive sparse review
				remains available.
			</p>
		{/if}

		{#if exportError}
			<div
				role="alert"
				class="rounded-sm border border-destructive/60 bg-destructive/10 px-3 py-2 text-xs text-destructive"
			>
				{exportError}
			</div>
		{/if}

		<Button
			data-timelapse-generate
			class="h-11 w-full md:h-9"
			disabled={!canGenerate}
			onclick={() => void startTimelapseExport()}
		>
			{submittingExport ? 'Starting silent MP4…' : 'Generate Silent MP4'}
		</Button>

		{#if activeJob}
			<div
				data-timelapse-active-job
				data-job-status={activeJob.status}
				class="space-y-2 rounded-sm border border-hairline bg-raised p-3 text-xs"
			>
				<div class="flex items-center justify-between">
					<span class="font-semibold capitalize">{activeJob.status}</span>
					<span class="font-mono">{Math.round(activeJob.progress * 100)}%</span>
				</div>
				{#if activeJob.error}
					<p class="text-destructive">{activeJob.error}</p>
				{/if}
				<div class="flex flex-wrap gap-2">
					{#if activeJob.status === 'running'}
						<Button
							data-timelapse-cancel-job
							size="sm"
							variant="outline"
							onclick={() => activeJob && void cancelJob(activeJob.id)}
						>
							<XIcon class="mr-1 size-3.5" />
							Cancel
						</Button>
					{/if}
					{#if activeJob.retryable && (activeJob.status === 'failed' || activeJob.status === 'cancelled')}
						<Button
							data-timelapse-retry-job
							size="sm"
							variant="outline"
							onclick={() => activeJob && void retryJob(activeJob.id)}
						>
							<RotateCcwIcon class="mr-1 size-3.5" />
							Retry
						</Button>
					{/if}
					{#if activeJob.status === 'ready'}
						<Button
							data-timelapse-download-mp4
							size="sm"
							disabled={downloadingId !== null}
							onclick={() => activeJob && void downloadArtifact(activeJob.id, 'media')}
						>
							<DownloadIcon class="mr-1 size-3.5" />
							Download MP4
						</Button>
						<Button
							data-timelapse-download-manifest
							size="sm"
							variant="outline"
							disabled={downloadingId !== null}
							onclick={() => activeJob && void downloadArtifact(activeJob.id, 'manifest')}
						>
							<FileJsonIcon class="mr-1 size-3.5" />
							Manifest JSON
						</Button>
					{/if}
				</div>
			</div>
		{/if}

		{#if timelapseJobs.length > 0}
			<div class="space-y-2 border-t border-hairline pt-3">
				<h3 class="text-xs font-semibold text-text-muted">Recent Timelapse Exports</h3>
				<ul class="space-y-1.5 text-xs">
					{#each timelapseJobs.slice(0, 5) as item (item.id)}
						<li class="flex items-center justify-between gap-2 rounded-sm bg-raised px-2 py-1.5">
							<div class="min-w-0">
								<div class="truncate font-mono text-2xs">{item.fileName ?? item.id}</div>
								<div class="text-2xs text-text-muted capitalize">
									{item.status} · {item.timelapse?.sampleCount ?? 0} frames
								</div>
							</div>
							{#if item.status === 'ready'}
								<div class="flex shrink-0 gap-1">
									<Button
										size="icon"
										variant="ghost"
										class="size-8"
										title="Download silent MP4"
										onclick={() => void downloadArtifact(item.id, 'media')}
									>
										<DownloadIcon class="size-3.5" />
									</Button>
									<Button
										size="icon"
										variant="ghost"
										class="size-8"
										title="Download JSON manifest"
										onclick={() => void downloadArtifact(item.id, 'manifest')}
									>
										<FileJsonIcon class="size-3.5" />
									</Button>
								</div>
							{/if}
						</li>
					{/each}
				</ul>
			</div>
		{/if}
	</aside>
</section>
