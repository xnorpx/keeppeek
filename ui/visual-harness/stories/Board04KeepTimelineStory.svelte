<script lang="ts">
	import VerticalTimeline from '$lib/components/VerticalTimeline.svelte';
	import type { RecordingEvent, RecordingSegment } from '$lib/types';
	import BellIcon from '@lucide/svelte/icons/bell';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import PlayIcon from '@lucide/svelte/icons/play';
	import VideoIcon from '@lucide/svelte/icons/video';

	const date = '2026-08-18';
	const dayStartMs = Date.parse(`${date}T00:00:00Z`);
	const nowMs = Date.parse(`${date}T06:51:44Z`);
	const playheadMs = Date.parse(`${date}T06:37:23Z`);
	const at = (time: string) => Date.parse(`${date}T${time}Z`);
	const segments: RecordingSegment[] = [
		['04:00:00', '04:39:00'],
		['04:43:00', '05:18:00'],
		['05:24:00', '05:44:00'],
		['05:48:00', '06:08:00'],
		['06:12:00', '06:51:44']
	].map(([start, end], index) => ({
		stream: 'main',
		date,
		hour: start.slice(0, 2),
		filename: `front-door-${index}.mp4`,
		url: `/story/keep/front-door-${index}.mp4`,
		start_time_ms: at(start),
		end_time_ms: at(end),
		duration_ms: at(end) - at(start)
	}));

	function event(
		id: string,
		kind: string,
		start: string,
		options: { source?: RecordingEvent['source']; confidence?: number | null; image?: boolean } = {}
	): RecordingEvent {
		return {
			id,
			source: options.source ?? 'camera',
			kind,
			start_time_ms: at(start),
			end_time_ms: at(start) + 8 * 60_000,
			confidence: options.confidence ?? null,
			bbox: null,
			zone: null,
			thumbnail_url: options.image === false ? null : `/story/timeline/${id}.jpg`
		};
	}

	const events: RecordingEvent[] = [
		event('person-now', 'person', '06:37:23', { confidence: 0.91 }),
		event('delivery', 'vehicle', '06:12:04', { confidence: 0.84 }),
		event('motion', 'motion', '05:48:51', { image: false }),
		event('car', 'vehicle', '05:23:36', { confidence: 0.79 }),
		event('gate', 'person', '04:43:33', { confidence: 0.88 }),
		event('story', 'story', '04:12:40', { source: 'keeppeek' })
	];
	const navigation = [VideoIcon, HistoryIcon, BellIcon, CameraIcon] as const;
</script>

<main
	data-paper-scenario="keep.desktop.timeline-anatomy"
	class="flex h-[720px] w-[1280px] shrink-0 overflow-hidden rounded-lg border border-hairline bg-ground [font-synthesis:none]"
>
	<aside
		class="flex h-[718px] w-16 shrink-0 flex-col items-center gap-[22px] border-r border-hairline bg-surface py-5"
		aria-label="Keep navigation preview"
	>
		<span class="size-[26px] shrink-0 rounded-sm bg-primary"></span>
		{#each navigation as Icon, index (index)}
			<Icon class="size-5 {index === 1 ? 'text-primary' : 'text-text-faint'}" strokeWidth={1.75} />
		{/each}
	</aside>

	<section
		class="flex h-[718px] w-[818px] shrink-0 flex-col"
		aria-label="Recorded video player preview"
	>
		<header class="flex h-[52px] shrink-0 items-center gap-3.5 border-b border-hairline px-5">
			<h1 class="text-[15px] leading-[18px] font-semibold">Front Door</h1>
			<span class="h-4 w-px bg-hairline"></span>
			<span class="font-mono text-xs leading-4 text-text-muted">Tue 18 Aug 2026</span>
			<span class="flex-1"></span>
			<span
				class="rounded-sm border border-hairline bg-raised px-2.5 py-[5px] font-mono text-[11px] tracking-[0.08em] text-text-muted"
				>MAIN</span
			>
			<span
				class="rounded-sm border border-hairline bg-raised px-2.5 py-[5px] font-mono text-[11px] tracking-[0.08em] text-text-muted"
				>1.0×</span
			>
		</header>
		<div class="flex h-[666px] shrink-0 flex-col justify-between bg-video p-4">
			<div class="flex items-start justify-between">
				<div class="rounded-sm bg-video/80 px-[11px] py-2 font-mono text-white">
					<p class="text-[13px] leading-4">2026-08-18 06:37:23.412</p>
					<p class="mt-1.5 text-[11px] leading-[14px] tracking-[0.08em] text-white/60">
						3840×2160 · 25 FPS · HEVC
					</p>
				</div>
				<span
					class="inline-flex items-center gap-[7px] rounded-full bg-video/80 px-[11px] py-1.5 font-mono text-[11px] leading-[14px] tracking-[0.14em] text-white"
					><span class="size-[7px] rounded-full bg-white/85"></span>REC</span
				>
			</div>
			<p class="text-center font-mono text-[11px] leading-[14px] tracking-[0.14em] text-text-faint">
				RECORDED VIDEO SURFACE · DETERMINISTIC MEDIA OMITTED
			</p>
			<div class="flex h-[38px] items-center gap-3.5 rounded-md bg-video/80 px-3 text-white/70">
				<PlayIcon class="size-[18px] text-white" fill="currentColor" />
				<span class="font-mono text-xs">|◀</span><span class="font-mono text-xs">▶|</span>
				<span class="h-[3px] flex-1 rounded-full bg-white/20"></span>
				<span class="font-mono text-[11px]">06:37:23 / 24:00:00</span>
			</div>
		</div>
	</section>

	<VerticalTimeline
		{segments}
		{events}
		selectedUrl={segments[4].url}
		{playheadMs}
		{dayStartMs}
		{nowMs}
		onSeek={() => {}}
		paperFrame
	/>
</main>
