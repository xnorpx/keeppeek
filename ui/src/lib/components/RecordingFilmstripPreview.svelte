<script lang="ts">
	import type { CameraListItem, RecordingSegment } from '$lib/types';
	import VideoOffIcon from '@lucide/svelte/icons/video-off';

	const timeFormatter = new Intl.DateTimeFormat(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		hour12: false,
		timeZone: 'UTC'
	});

	type Props = {
		camera: CameraListItem;
		segment: RecordingSegment | null;
		timestampMs: number | null;
		playing: boolean;
		playbackRate: number;
		onselect: (cameraId: string) => void;
	};

	let { camera, segment, timestampMs, playing, playbackRate, onselect }: Props = $props();
	let container = $state<HTMLElement | null>(null);
	let video = $state<HTMLVideoElement | null>(null);
	let visible = $state(false);
	let failed = $state(false);
	let cameraName = $derived(camera.name ?? camera.id);
	let previewTimeMs = $derived(timestampMs ?? segment?.start_time_ms ?? null);

	function synchronizeVideo(
		node: HTMLVideoElement,
		activeSegment: RecordingSegment,
		targetMs: number
	) {
		const segmentSeconds = activeSegment.duration_ms / 1_000;
		const latestSecond = Math.max(0, segmentSeconds - 0.05);
		const offsetSeconds = Math.max(
			0,
			Math.min(latestSecond, (targetMs - activeSegment.start_time_ms) / 1_000)
		);
		const driftTolerance = playing ? 0.4 : 0.05;
		if (Math.abs(node.currentTime - offsetSeconds) > driftTolerance) {
			node.currentTime = offsetSeconds;
		}
		if (node.playbackRate !== playbackRate) node.playbackRate = playbackRate;
		if (playing) {
			if (node.paused) void node.play().catch(() => {});
		} else if (!node.paused) {
			node.pause();
		}
	}

	function synchronizeCurrentVideo() {
		if (!visible || !video || !segment || previewTimeMs === null) return;
		synchronizeVideo(video, segment, previewTimeMs);
	}

	$effect(() => {
		const node = container;
		if (!node) return;
		if (!('IntersectionObserver' in window)) {
			visible = true;
			return;
		}
		const observer = new IntersectionObserver(
			([entry]) => {
				visible = entry?.isIntersecting ?? false;
			},
			{ threshold: 0.25 }
		);
		observer.observe(node);
		return () => observer.disconnect();
	});

	$effect(() => {
		const node = video;
		const activeSegment = segment;
		const targetMs = previewTimeMs;
		const inView = visible;
		if (!node) return;
		if (!inView) {
			if (!node.paused) node.pause();
			return;
		}
		if (!activeSegment || targetMs === null) return;
		if (node.readyState < 1) {
			node.load();
			return;
		}
		synchronizeVideo(node, activeSegment, targetMs);
	});
</script>

<article
	bind:this={container}
	class="relative aspect-video w-44 shrink-0 overflow-hidden rounded-md border border-border/70 bg-black sm:w-52"
>
	{#if segment}
		<video
			bind:this={video}
			muted
			playsinline
			preload={visible ? 'metadata' : 'none'}
			src={segment.url}
			class="pointer-events-none size-full object-cover"
			aria-hidden="true"
			data-recording-preview={camera.id}
			onloadedmetadata={synchronizeCurrentVideo}
			onerror={() => (failed = true)}
		></video>
		{#if failed}
			<div
				class="pointer-events-none absolute inset-0 grid place-items-center bg-zinc-950 text-white/45"
			>
				<VideoOffIcon class="size-5" />
			</div>
		{/if}
	{:else}
		<div
			class="pointer-events-none absolute inset-0 grid place-items-center bg-zinc-950 text-white/35"
		>
			<VideoOffIcon class="size-5" />
		</div>
	{/if}

	<button
		type="button"
		class="absolute inset-0 z-10 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset disabled:cursor-not-allowed"
		disabled={!segment}
		aria-label={segment && previewTimeMs !== null
			? `Review ${cameraName} at ${timeFormatter.format(new Date(previewTimeMs))} UTC`
			: `${cameraName} has no recording at this time`}
		onclick={() => onselect(camera.id)}
	></button>

	<div
		class="pointer-events-none absolute inset-x-0 bottom-0 z-20 flex items-center justify-between gap-2 bg-black/75 px-2 py-1.5 text-white backdrop-blur-sm"
	>
		<span class="truncate text-[10px] font-semibold sm:text-[11px]">{cameraName}</span>
		{#if segment && previewTimeMs !== null}
			<time class="shrink-0 font-mono text-[9px] text-white/60 tabular-nums">
				{timeFormatter.format(new Date(previewTimeMs))}
			</time>
		{:else}
			<span class="shrink-0 text-[9px] text-white/45">No recording</span>
		{/if}
	</div>
</article>
