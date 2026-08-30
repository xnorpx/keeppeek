<script lang="ts">
	import {
		formatAge,
		formatBytes,
		formatDuration,
		formatPercent,
		formatTimestamp,
		gapCauseLabel,
		writerStateLabel
	} from '$lib/recording-coverage';
	import type { CameraRecordingCoverage } from '$lib/types';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import ArrowLeftToLineIcon from '@lucide/svelte/icons/arrow-left-to-line';
	import ArrowRightToLineIcon from '@lucide/svelte/icons/arrow-right-to-line';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import ScrollTextIcon from '@lucide/svelte/icons/scroll-text';
	import RecordingCoverageStrip from './RecordingCoverageStrip.svelte';

	type Props = {
		camera: CameraRecordingCoverage;
		windowStartMs: number;
		windowEndMs: number;
		nowMs: number;
	};

	let { camera, windowStartMs, windowEndMs, nowMs }: Props = $props();
	let requestedStreamId = $state<string | null>(null);
	let selectedStream = $derived(
		camera.streams.find((stream) => stream.stream_id === requestedStreamId) ??
			camera.streams.find((stream) => stream.recording_requested) ??
			camera.streams[0] ??
			null
	);

	function writerTone(state: string): string {
		if (state === 'progressing') return 'text-healthy';
		if (state === 'stalled' || state === 'pending') return 'text-activity';
		if (state === 'failed') return 'text-live-text';
		return 'text-text-muted';
	}
</script>

<section
	data-recording-camera-detail
	class="min-w-0 border-t border-hairline bg-surface xl:h-full xl:min-h-0 xl:overflow-y-auto xl:border-t-0 xl:border-l"
	aria-labelledby="recording-camera-heading"
>
	<header class="flex min-h-16 items-center gap-3 border-b border-hairline px-4 py-3">
		<div class="min-w-0 flex-1">
			<p class="font-mono text-2xs tracking-caps text-text-faint">CAMERA DETAIL</p>
			<h2 id="recording-camera-heading" class="truncate text-base font-semibold">
				{camera.camera_name}
			</h2>
		</div>
		<a
			href={camera.health_href}
			class="inline-flex h-11 items-center gap-2 rounded-sm border border-hairline-strong px-3 text-xs text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:h-9"
		>
			<ActivityIcon class="size-3.5" /> Health
		</a>
	</header>

	{#if camera.streams.length === 0}
		<div class="px-4 py-10 text-sm text-text-muted">No recording stream is configured.</div>
	{:else if selectedStream}
		<div
			class="flex min-h-11 items-center gap-1 overflow-x-auto border-b border-hairline px-4"
			role="tablist"
			aria-label="Recording stream"
		>
			{#each camera.streams as stream (stream.stream_id)}
				<button
					type="button"
					role="tab"
					aria-selected={selectedStream.stream_id === stream.stream_id}
					class="h-8 shrink-0 rounded-sm px-3 font-mono text-2xs uppercase focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {selectedStream.stream_id ===
					stream.stream_id
						? 'bg-primary text-primary-foreground'
						: 'text-text-muted hover:bg-raised'}"
					onclick={() => (requestedStreamId = stream.stream_id)}
				>
					{stream.stream_id}
				</button>
			{/each}
		</div>

		<div class="border-b border-hairline px-4 py-4">
			<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
				<span
					class="size-2 rounded-full {selectedStream.writer_state === 'progressing'
						? 'bg-healthy'
						: selectedStream.writer_state === 'failed'
							? 'bg-live'
							: 'bg-activity'}"
				></span>
				<p class="text-sm font-semibold {writerTone(selectedStream.writer_state)}">
					{writerStateLabel(selectedStream.writer_state)}
				</p>
				<span class="font-mono text-2xs text-text-faint uppercase">
					{selectedStream.recording_requested
						? `${camera.policy} policy`
						: 'No recording requested'}
				</span>
			</div>
		</div>

		<div
			class="grid grid-cols-2 border-b border-hairline xl:grid-cols-2 sm:grid-cols-4 2xl:grid-cols-4"
		>
			{#each [['LAST FRAME', formatAge(selectedStream.last_frame_at_ms, nowMs), formatTimestamp(selectedStream.last_frame_at_ms)], ['LAST WRITE', formatAge(selectedStream.last_write_at_ms, nowMs), formatTimestamp(selectedStream.last_write_at_ms)], ['LAST FINALIZE', formatAge(selectedStream.last_finalize_at_ms, nowMs), formatTimestamp(selectedStream.last_finalize_at_ms)], ['CATALOG COMMIT', formatAge(selectedStream.last_catalog_commit_at_ms, nowMs), formatTimestamp(selectedStream.last_catalog_commit_at_ms)]] as metric (metric[0])}
				<div
					class="min-w-0 border-r border-b border-hairline px-3 py-3 even:border-r-0 xl:even:border-r-0 sm:even:border-r 2xl:even:border-r"
				>
					<p class="font-mono text-[10px] tracking-caps text-text-faint">{metric[0]}</p>
					<p class="mt-1 truncate text-xs font-semibold">{metric[1]}</p>
					<p class="mt-0.5 truncate font-mono text-[10px] text-text-faint">{metric[2]}</p>
				</div>
			{/each}
		</div>

		<div class="border-b border-hairline px-4 py-4">
			<div class="mb-3 flex items-end justify-between gap-3">
				<div>
					<p class="font-mono text-2xs tracking-caps text-text-faint">SELECTED COVERAGE</p>
					<p class="mt-1 text-2xl font-semibold tabular-nums">
						{formatPercent(selectedStream.coverage_percent)}
					</p>
				</div>
				<div class="text-right text-xs text-text-muted">
					<p>{formatDuration(selectedStream.selected_coverage_ms)} playable</p>
					<p>
						{selectedStream.gap_count} gaps · largest {formatDuration(
							selectedStream.largest_gap_ms
						)}
					</p>
				</div>
			</div>
			<RecordingCoverageStrip stream={selectedStream} {windowStartMs} {windowEndMs} />
		</div>

		<div
			class="grid grid-cols-2 border-b border-hairline xl:grid-cols-2 sm:grid-cols-4 2xl:grid-cols-4"
		>
			{#each [['OLDEST', formatTimestamp(selectedStream.oldest_retained_at_ms)], ['NEWEST', formatTimestamp(selectedStream.newest_retained_at_ms)], ['RETENTION', formatDuration(selectedStream.effective_retention_ms)], ['STORAGE', formatBytes(selectedStream.recording_bytes)]] as metric (metric[0])}
				<div
					class="min-w-0 border-r border-hairline px-3 py-3 even:border-r-0 xl:even:border-r-0 sm:even:border-r 2xl:even:border-r"
				>
					<p class="font-mono text-[10px] tracking-caps text-text-faint">{metric[0]}</p>
					<p class="mt-1 truncate text-xs font-semibold">{metric[1]}</p>
					{#if metric[0] === 'STORAGE'}
						<p class="mt-0.5 truncate font-mono text-[10px] text-text-faint">
							{formatBytes(selectedStream.estimated_bytes_per_day)} / DAY
						</p>
					{/if}
				</div>
			{/each}
		</div>

		<section aria-labelledby="recording-gaps-heading">
			<header class="flex min-h-12 items-center justify-between border-b border-hairline px-4">
				<h3 id="recording-gaps-heading" class="text-sm font-semibold">Recording gaps</h3>
				<span class="font-mono text-2xs text-text-faint">{selectedStream.gaps.length} SHOWN</span>
			</header>
			{#if !selectedStream.recording_requested}
				<div class="border-b border-hairline px-4 py-4 text-sm text-text-muted">
					Recording is not requested by the effective policy. This is distinct from an unexpected
					gap.
				</div>
			{:else if selectedStream.gaps.length === 0}
				<div class="border-b border-hairline px-4 py-4 text-sm text-healthy">
					No gap meets the selected minimum duration.
				</div>
			{:else}
				{#each selectedStream.gaps
					.slice(-12)
					.reverse() as gap (`${gap.start_ms}-${gap.observed_end_ms}`)}
					<article data-recording-gap class="border-b border-hairline px-4 py-3">
						<div class="flex items-start gap-3">
							<span
								class="mt-1.5 size-2 shrink-0 rounded-full {gap.end_ms === null
									? 'bg-live'
									: 'bg-activity'}"
							></span>
							<div class="min-w-0 flex-1">
								<div class="flex flex-wrap items-baseline gap-x-2 gap-y-1">
									<p class="text-xs font-semibold">{gapCauseLabel(gap.cause)}</p>
									<span class="font-mono text-[10px] text-text-faint uppercase"
										>{gap.end_ms === null ? 'Open' : formatDuration(gap.duration_ms)}</span
									>
								</div>
								<p class="mt-1 text-xs leading-4 text-text-muted">{gap.explanation}</p>
								<p class="mt-1 font-mono text-[10px] text-text-faint">
									{formatTimestamp(gap.start_ms)} → {gap.end_ms === null
										? 'NOW'
										: formatTimestamp(gap.end_ms)}
								</p>
							</div>
							<div class="flex shrink-0 items-center gap-1">
								{#if gap.before_href}
									<a
										href={gap.before_href}
										class="grid size-11 place-items-center rounded-sm border border-hairline-strong text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:size-9"
										title="Open footage before gap"
										aria-label="Open footage before gap"
									>
										<ArrowLeftToLineIcon class="size-3.5" />
									</a>
								{/if}
								{#if gap.after_href}
									<a
										href={gap.after_href}
										class="grid size-11 place-items-center rounded-sm border border-hairline-strong text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:size-9"
										title="Open footage after gap"
										aria-label="Open footage after gap"
									>
										<ArrowRightToLineIcon class="size-3.5" />
									</a>
								{/if}
								<a
									href={gap.health_href}
									class="grid size-11 place-items-center rounded-sm border border-hairline-strong text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:size-9"
									title="Open supporting health evidence"
									aria-label="Open supporting health evidence"
								>
									<ExternalLinkIcon class="size-3.5" />
								</a>
								<a
									href={gap.logs_href}
									class="grid size-11 place-items-center rounded-sm border border-hairline-strong text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:size-9"
									title="Open relevant logs"
									aria-label="Open relevant logs"
								>
									<ScrollTextIcon class="size-3.5" />
								</a>
							</div>
						</div>
					</article>
				{/each}
			{/if}
		</section>
	{/if}
</section>
