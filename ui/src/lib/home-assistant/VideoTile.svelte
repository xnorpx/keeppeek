<script lang="ts">
	import Maximize2 from '@lucide/svelte/icons/maximize-2';
	import Play from '@lucide/svelte/icons/play';
	import VideoOff from '@lucide/svelte/icons/video-off';
	import type { StreamState } from './connection-manager';

	type Props = {
		title: string;
		state?: StreamState;
		ratio: string;
		showName: boolean;
		onfocus?: () => void;
	};
	let { title, state: streamState, ratio, showName, onfocus }: Props = $props();
	let video: HTMLVideoElement | undefined = $state();
	let playing = $state(false);
	let blocked = $state(false);
	let playbackError = $state(false);
	let stream = $derived(streamState?.stream ?? null);

	$effect(() => {
		const element = video;
		if (!element) return;
		element.srcObject = stream;
		playing = false;
		blocked = false;
		playbackError = false;
		let cancelled = false;
		if (stream)
			void element.play().catch(() => {
				if (!cancelled) blocked = true;
			});
		return () => {
			cancelled = true;
			element.pause();
			element.srcObject = null;
		};
	});
	function play() {
		void video
			?.play()
			.then(() => {
				blocked = false;
			})
			.catch(() => {
				playbackError = true;
			});
	}
</script>

<figure class="video-tile">
	<div class="video-surface" style:aspect-ratio={ratio.replace(':', ' / ')}>
		<video
			bind:this={video}
			aria-label={`Live video: ${title}`}
			autoplay
			muted
			playsinline
			onplaying={() => {
				playing = true;
			}}
			onwaiting={() => {
				playing = false;
			}}
			onerror={() => {
				playbackError = true;
			}}
		></video>
		{#if streamState?.status === 'unavailable' || playbackError}
			<div class="video-state" role="status">
				<VideoOff size={24} /><span
					>{playbackError
						? 'Video playback failed. Check browser codec support.'
						: streamState?.message}</span
				>
			</div>
		{:else if blocked}
			<div class="video-state">
				<button type="button" onclick={play}><Play size={18} />Play</button>
			</div>
		{:else if !playing}
			<div class="video-state" role="status">
				<span class="loading-line"></span><span>Waiting for live video</span>
			</div>
		{/if}
	</div>
	<figcaption>
		{#if showName}<strong>{title}</strong>{/if}
		<span class:live={playing} class="tile-status"
			>{playing ? 'Live' : streamState?.status === 'unavailable' ? 'Offline' : 'Waiting'}</span
		>
		{#if onfocus}<button
				type="button"
				class="icon-button"
				aria-label={`Focus ${title}`}
				title="Focus camera"
				onclick={onfocus}><Maximize2 size={16} /></button
			>{/if}
	</figcaption>
</figure>
