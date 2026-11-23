<script lang="ts">
	import { untrack } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import type { RecordedReviewRequests } from '$lib/recorded-review-requests';
	import type { StoredMediaPlayback } from '$lib/control-client';
	import { needsGridDriftCorrection } from '$lib/grid-playback-session';

	let {
		onremove,
		onvisibility,
		requests,
		cameraId,
		name,
		stream,
		endMs,
		epoch,
		revision,
		position,
		playing,
		rate,
		muted,
		status
	}: {
		onremove: () => void;
		onvisibility: (fraction: number) => void;
		requests: RecordedReviewRequests;
		cameraId: string;
		name: string;
		stream: 'main' | 'sub' | null;
		endMs: number;
		epoch: number;
		revision: number;
		position: () => number;
		playing: boolean;
		rate: number;
		muted: boolean;
		status: string;
	} = $props();
	const client = useControlClient();
	let video = $state<HTMLVideoElement>();
	let playback = $state.raw<StoredMediaPlayback | null>(null);
	let source = $state<string>();
	let issue = $state('');
	let ready = $state(false);
	let tail = Promise.resolve();
	const correctionIntervalMs = 750;
	let lastCorrection = 0;
	let tile = $state<HTMLElement>();
	let attempt = $state(0);
	let admittedEnd = $state<number | null>(null);
	$effect(() => {
		// A later catalog page can extend the same recording while this finite cursor is healthy.
		// Renew only at its actual limit, or immediately if refreshed coverage becomes shorter.
		if (admittedEnd === null || !stream || endMs === admittedEnd) return;
		if (endMs < admittedEnd || epoch >= admittedEnd) {
			admittedEnd = null;
			attempt++;
		}
	});
	$effect(() => {
		if (!tile) return;
		const observer = new IntersectionObserver(
			([entry]) => {
				if (entry) onvisibility(entry.intersectionRatio);
			},
			{ threshold: [0, 0.01, 1 / 3, 1] }
		);
		observer.observe(tile);
		return () => observer.disconnect();
	});

	let presented = $state<number | null>(null);
	let correctionMs = $state<number | null>(null);

	$effect(() => {
		const element = video,
			cursor = playback;
		presented = null;
		correctionMs = null;
		if (!element || !cursor || !element.requestVideoFrameCallback) return;
		let handle = 0;
		let retry: ReturnType<typeof setTimeout> | undefined;
		let last = -Infinity;
		function frame(now: number, data: VideoFrameCallbackMetadata) {
			if (!element || !cursor || element !== video || cursor !== playback) return;
			const actual = Math.round(cursor.anchorTimeMs + data.mediaTime * 1000);
			const drift = Math.round(position() - actual);
			const outsideTolerance = needsGridDriftCorrection(drift);
			// Publish the first real frame on either side of a correction immediately.
			if (now - last >= 100 || element.paused || outsideTolerance !== (correctionMs !== null)) {
				presented = actual;
				correctionMs = outsideTolerance ? drift : null;
				last = now;
			}
			if (element.paused && outsideTolerance && retry === undefined) {
				const remaining = Math.max(0, lastCorrection + correctionIntervalMs - performance.now());
				retry = setTimeout(() => {
					retry = undefined;
					if (cursor === playback && element === video && element.paused && correctionMs !== null) {
						align(false, true);
					}
				}, remaining + 1);
			}
			handle = element.requestVideoFrameCallback(frame);
		}
		handle = element.requestVideoFrameCallback(frame);
		return () => {
			element.cancelVideoFrameCallback(handle);
			clearTimeout(retry);
		};
	});

	$effect(() => {
		const id = cameraId,
			selected = stream,
			end = untrack(() => endMs);
		void revision;
		void attempt;
		const abort = new AbortController();
		let acquired: StoredMediaPlayback | undefined;
		let off: (() => void) | undefined;
		const target = Math.floor(untrack(position));
		source = undefined;
		playback = null;
		issue = '';
		ready = false;
		admittedEnd = selected ? end : null;
		if (!selected) return;
		tail = tail.then(async () => {
			if (abort.signal.aborted) return;
			try {
				acquired = await client.openStoredMedia({
					schedule: (operation) => requests.run(operation, abort.signal),
					sourceId: id,
					streamId: selected,
					timestampMs: target,
					endTimeMs: end,
					playing,
					playbackRate: rate,
					signal: abort.signal
				});
				if (abort.signal.aborted) return;
				const opened = acquired;
				playback = opened;
				source = opened.url;
				off = opened.onError((message) => failPlayback(opened, message || 'Recording unavailable'));
			} catch (cause) {
				if (!abort.signal.aborted)
					issue = cause instanceof Error ? cause.message : 'Recording unavailable';
			}
		});
		return () => {
			abort.abort();
			off?.();
			tail = tail.then(async () => {
				await acquired?.close().catch(() => undefined);
			});
		};
	});

	$effect(() => {
		const current = playback;
		if (!current) return;
		current.setPlaying(playing);
		current.setPlaybackRate(rate);
		if (!ready || !video) return;
		video.playbackRate = rate;
		if (playing)
			void video.play().catch(() => {
				if (playing) failPlayback(current, 'Playback interrupted');
			});
		else video.pause();
	});

	function failPlayback(cursor: StoredMediaPlayback, message: string) {
		if (playback !== cursor) return;
		issue = message;
		ready = false;
		playback = null;
		source = undefined;
		tail = tail.then(() => cursor.close().catch(() => undefined));
	}

	function align(initial = false, pausedCorrection = false) {
		if (!playback || !video || !video.buffered.length) return;
		const offset = Math.max(0, (position() - playback.anchorTimeMs) / 1000);
		const driftMs = (offset - video.currentTime) * 1000;
		if (
			initial ||
			((playing || pausedCorrection) &&
				needsGridDriftCorrection(driftMs) &&
				performance.now() - lastCorrection >= correctionIntervalMs)
		) {
			for (let index = 0; index < video.buffered.length; index++) {
				if (offset >= video.buffered.start(index) && offset < video.buffered.end(index)) {
					if (Math.abs(driftMs) > 1) video.currentTime = offset;
					lastCorrection = performance.now();
					break;
				}
			}
		}
		playback.observe(video.currentTime);
	}
</script>

<article
	bind:this={tile}
	class="min-w-0 overflow-hidden rounded-sm border border-hairline bg-raised"
	aria-label={name}
>
	<header class="flex flex-wrap items-center justify-between gap-2 px-3 py-2">
		<h2 class="text-sm font-semibold">{name}</h2>
		{#if presented !== null}
			<time class="font-mono text-xs text-text-muted" datetime={new Date(presented).toISOString()}>
				{new Date(presented).toISOString().slice(11, 23)} UTC
			</time>
		{/if}
		{#if correctionMs !== null}
			<span class="text-xs text-text-muted">
				<span role="status">Synchronizing</span>
				{Math.abs(correctionMs)} ms {correctionMs > 0 ? 'behind' : 'ahead'}
			</span>
		{/if}
		<button class="min-h-11 px-2 text-sm underline" aria-label={'Remove ' + name} onclick={onremove}
			>Remove</button
		>
	</header>
	{#if source && !issue}
		<!-- svelte-ignore a11y_media_has_caption -- Recorded camera footage has no authored captions. -->
		<video
			bind:this={video}
			src={source}
			{muted}
			playsinline
			preload="auto"
			class="aspect-video w-full bg-black object-contain"
			onloadeddata={() => {
				align(true);
				ready = true;
			}}
			onerror={(event) => {
				if (event.currentTarget === video && video.currentSrc === source && playback)
					failPlayback(playback, 'The browser could not play this recording.');
			}}
			ontimeupdate={() => align()}
		></video>
	{:else}
		<p class="grid aspect-video place-content-center p-3 text-sm text-text-muted" role="status">
			{issue || status}
			{#if issue}<button class="min-h-11 underline" onclick={() => attempt++}
					>Retry recording</button
				>{/if}
		</p>
	{/if}
</article>
