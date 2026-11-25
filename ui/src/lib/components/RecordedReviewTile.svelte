<script lang="ts">
	import { untrack } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import type { RecordedReviewRequests } from '$lib/recorded-review-requests';
	import { reviewAudioStatusLabel } from '$lib/recorded-review-requests';
	import type { StoredMediaPlayback } from '$lib/control-client';
	import type { RecordedAudioTiming } from '$lib/proto/webrtc_pb';
	import { RecordedFramePlayer } from '$lib/recorded-frame-player.svelte';
	import type { PresentedStoredFrame } from '$lib/stored-frame-step';
	import { gridPlaybackRateForDrift, needsGridDriftCorrection } from '$lib/grid-playback-session';

	export type ReviewStepState = {
		available: boolean;
		busy: boolean;
		notice: string;
		timeNs?: bigint;
	};

	let {
		onremove,
		onvisibility,
		onstep,
		onstepstate,
		requests,
		cameraId,
		name,
		stream,
		encoding = null,
		fallbackNotice = '',
		endMs,
		epoch,
		revision,
		position,
		playing,
		rate,
		muted,
		isAudioSource = false,
		audioMuted = false,
		focused = false,
		status
	}: {
		onremove: () => void;
		onvisibility: (fraction: number) => void;
		onstep?: (timestampMs: number) => void;
		onstepstate?: (state: ReviewStepState) => void;
		requests: RecordedReviewRequests;
		cameraId: string;
		name: string;
		stream: 'main' | 'sub' | null;
		encoding?: string | null;
		fallbackNotice?: string;
		endMs: number;
		epoch: number;
		revision: number;
		position: () => number;
		playing: boolean;
		rate: number;
		muted: boolean;
		isAudioSource?: boolean;
		audioMuted?: boolean;
		focused?: boolean;
		status: string;
	} = $props();
	const client = useControlClient();
	const frames = new RecordedFramePlayer();
	let video = $state<HTMLVideoElement>();
	let frameCanvas = $state<HTMLCanvasElement>();
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
	let audioTiming = $state.raw<RecordedAudioTiming | undefined>();
	let presented = $state<number | null>(null);
	let driftMs = $state<number | null>(null);
	let correctionMs = $state<number | null>(null);
	let correctionCount = $state(0);
	let bufferedSeconds = $state<number | null>(null);
	let retryAlignment = $state(false);

	const variantLabel = $derived.by(() => {
		if (!stream) return '';
		const streamName = stream === 'main' ? 'Main' : 'Sub';
		const codec = encoding?.trim();
		return codec ? `${streamName} · ${codec.toUpperCase()}` : streamName;
	});
	const audioStatus = $derived(
		reviewAudioStatusLabel({
			isAudioSource,
			muted: audioMuted,
			audioPresent: audioTiming?.audioPresent,
			appliedOffsetMs: audioTiming?.appliedOffsetMs
		})
	);
	const showAudioBadge = $derived(
		Boolean(source && !issue && (isAudioSource || audioTiming?.audioPresent === false))
	);
	const canStep = $derived(
		Boolean(
			focused &&
			!playing &&
			ready &&
			source &&
			playback?.canStepExactly &&
			!frames.unsupported &&
			(frames.selection !== undefined || frames.displayedNs !== undefined)
		)
	);

	export function stepFrame(direction: -1 | 1): void {
		if (!focused || playing || !playback || !video?.paused || !frameCanvas) return;
		void frames.step(direction, playback, presentSteppedFrame);
	}

	function presentSteppedFrame(sample: PresentedStoredFrame): void {
		if (!frameCanvas || !video?.paused) throw new Error('The paused player changed.');
		const context = frameCanvas.getContext('2d');
		if (!context) throw new Error('The browser could not display this picture.');
		frameCanvas.width = sample.frame.displayWidth;
		frameCanvas.height = sample.frame.displayHeight;
		context.drawImage(sample.frame, 0, 0);
		const steppedMs =
			Number(sample.storedTimeNs / 1_000_000n) + Number(sample.storedTimeNs % 1_000_000n) / 1e6;
		presented = Math.round(steppedMs);
		driftMs = 0;
		correctionMs = null;
		retryAlignment = false;
		onstep?.(Math.floor(steppedMs));
	}

	$effect(() => {
		if (!focused) return;
		onstepstate?.({
			available: canStep,
			busy: frames.busy,
			notice: frames.notice,
			timeNs: frames.selection?.storedTimeNs
		});
	});

	$effect.pre(() => {
		const element = video;
		const cursor = playback;
		const url = source;
		if (!focused || !element || !cursor || !url) {
			untrack(() => frames.clear());
			return;
		}
		return untrack(() => frames.follow(element, cursor));
	});

	$effect(() => {
		if (playing && (frames.selection || frames.busy)) frames.clear();
	});

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

	$effect(() => {
		const element = video,
			cursor = playback;
		presented = null;
		driftMs = null;
		correctionMs = null;
		retryAlignment = false;
		if (!element || !cursor || !element.requestVideoFrameCallback) return;
		let handle = 0;
		let retry: ReturnType<typeof setTimeout> | undefined;
		let last = -Infinity;
		let pausedAttempts = 0;
		let lastAttempt = -Infinity;
		function schedulePausedCorrection() {
			if (retry !== undefined || !element?.paused || playing || frames.selection) return;
			const due = Math.max(lastCorrection, lastAttempt) + correctionIntervalMs;
			retry = setTimeout(
				() => {
					retry = undefined;
					if (
						cursor !== playback ||
						element !== video ||
						!element?.paused ||
						playing ||
						frames.selection ||
						correctionMs === null
					)
						return;
					if (pausedAttempts === 2) {
						retryAlignment = true;
						return;
					}
					pausedAttempts++;
					lastAttempt = performance.now();
					if (!element.seeking) align('paused-frame');
					schedulePausedCorrection();
				},
				Math.max(0, due - performance.now()) + 1
			);
		}
		function frame(now: number, data: VideoFrameCallbackMetadata) {
			if (!element || !cursor || element !== video || cursor !== playback) return;
			const actual = Math.round(cursor.anchorTimeMs + data.mediaTime * 1000);
			const drift = Math.round(position() - actual);
			const outsideTolerance = needsGridDriftCorrection(drift);
			// Publish the first real frame on either side of a correction immediately.
			if (
				!frames.selection &&
				(now - last >= 100 || element.paused || outsideTolerance !== (correctionMs !== null))
			) {
				presented = actual;
				driftMs = drift;
				correctionMs = outsideTolerance ? drift : null;
				last = now;
			}
			if (outsideTolerance && !frames.selection) {
				if (!retryAlignment) schedulePausedCorrection();
			} else {
				clearTimeout(retry);
				retry = undefined;
				pausedAttempts = 0;
				lastAttempt = -Infinity;
				retryAlignment = false;
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
		let offError: (() => void) | undefined;
		let offAudio: (() => void) | undefined;
		const target = Math.floor(untrack(position));
		source = undefined;
		playback = null;
		audioTiming = undefined;
		bufferedSeconds = null;
		correctionCount = 0;
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
				offError = opened.onError((message) =>
					failPlayback(opened, message || 'Recording unavailable')
				);
				offAudio = opened.onRecordedAudio((timing) => {
					if (playback === opened) audioTiming = timing;
				});
			} catch (cause) {
				if (!abort.signal.aborted)
					issue = cause instanceof Error ? cause.message : 'Recording unavailable';
			}
		});
		return () => {
			abort.abort();
			offError?.();
			offAudio?.();
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

	function forwardBufferSeconds(element: HTMLVideoElement): number {
		for (let index = 0; index < element.buffered.length; index++) {
			const start = element.buffered.start(index);
			const end = element.buffered.end(index);
			if (element.currentTime >= start - 0.05 && element.currentTime <= end) {
				return Math.max(0, end - element.currentTime);
			}
		}
		return 0;
	}

	function align(reason: 'playback' | 'load' | 'paused-frame' = 'playback') {
		if (!playback || !video || !video.buffered.length) return;
		bufferedSeconds = forwardBufferSeconds(video);
		const offset = Math.max(0, (position() - playback.anchorTimeMs) / 1000);
		const currentDriftMs = (offset - video.currentTime) * 1000;
		const pausedCorrection = reason === 'paused-frame';
		if (
			reason === 'load' ||
			((pausedCorrection || (playing && needsGridDriftCorrection(currentDriftMs))) &&
				performance.now() - lastCorrection >= correctionIntervalMs)
		) {
			for (let index = 0; index < video.buffered.length; index++) {
				if (offset >= video.buffered.start(index) && offset < video.buffered.end(index)) {
					// A seek may update currentTime before Chromium actually presents that picture.
					if (Math.abs(currentDriftMs) > 1 || pausedCorrection) {
						video.currentTime = offset;
						if (reason !== 'load') correctionCount++;
					}
					video.playbackRate = rate;
					lastCorrection = performance.now();
					break;
				}
			}
		} else if (playing && reason === 'playback') {
			const desiredRate = gridPlaybackRateForDrift(rate, currentDriftMs);
			if (Math.abs(video.playbackRate - desiredRate) > 0.001) video.playbackRate = desiredRate;
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
				<span role="status"
					>{retryAlignment && !playing
						? 'Alignment paused. Retry recording.'
						: 'Synchronizing'}</span
				>
				{Math.abs(correctionMs)} ms {correctionMs > 0 ? 'behind' : 'ahead'}
				{#if retryAlignment && !playing}<button
						class="min-h-11 px-2 underline"
						onclick={() => attempt++}>Retry recording</button
					>{/if}
			</span>
		{/if}
		<button class="min-h-11 px-2 text-sm underline" aria-label={'Remove ' + name} onclick={onremove}
			>Remove</button
		>
	</header>
	{#if source && !issue}
		<div class="relative aspect-video w-full bg-black">
			<!-- svelte-ignore a11y_media_has_caption -- Recorded camera footage has no authored captions. -->
			<video
				bind:this={video}
				src={source}
				{muted}
				playsinline
				preload="auto"
				class="size-full object-contain"
				onloadeddata={() => {
					align('load');
					ready = true;
				}}
				onerror={(event) => {
					if (event.currentTarget === video && video.currentSrc === source && playback)
						failPlayback(playback, 'The browser could not play this recording.');
				}}
				ontimeupdate={() => align()}
			></video>
			<canvas
				bind:this={frameCanvas}
				aria-label={'Selected recorded picture for ' + name}
				hidden={!frames.selection}
				class="pointer-events-none absolute inset-0 size-full bg-black object-contain"
			></canvas>
		</div>
		<footer
			class="flex flex-wrap items-center justify-between gap-2 px-3 py-1.5 text-xs text-text-muted"
		>
			<span data-review-variant>{variantLabel}</span>
			{#if fallbackNotice}<span data-review-fallback>{fallbackNotice}</span>{/if}
			{#if showAudioBadge}<span data-review-audio-state>{audioStatus}</span>{/if}
			<details class="ml-auto">
				<summary class="cursor-pointer py-1 underline">Diagnostics</summary>
				<dl class="mt-1 flex flex-wrap gap-x-4 gap-y-1 font-mono" data-review-diagnostics>
					<div
						><dt class="inline">Variant: </dt><dd class="inline">{variantLabel || 'None'}</dd></div
					>
					<div><dt class="inline">Audio: </dt><dd class="inline">{audioStatus}</dd></div>
					<div>
						<dt class="inline">Drift: </dt>
						<dd class="inline">{driftMs === null ? 'Pending' : `${driftMs} ms`}</dd>
					</div>
					<div>
						<dt class="inline">Buffer: </dt>
						<dd class="inline">
							{bufferedSeconds === null ? 'Pending' : `${bufferedSeconds.toFixed(2)} s`}
						</dd>
					</div>
					<div>
						<dt class="inline">Corrections: </dt>
						<dd class="inline">{correctionCount}</dd>
					</div>
				</dl>
			</details>
		</footer>
	{:else}
		<p class="grid aspect-video place-content-center p-3 text-sm text-text-muted" role="status">
			{issue || status}
			{#if issue}<button class="min-h-11 underline" onclick={() => attempt++}
					>Retry recording</button
				>{/if}
		</p>
	{/if}
</article>
