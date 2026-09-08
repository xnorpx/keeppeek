<script lang="ts">
	import '../../app.css';
	import FocusedMediaViewport from './FocusedMediaViewport.svelte';

	let {
		mediaKey = 'camera-a',
		aspectRatio = 16 / 9,
		enabled = true,
		playing = false,
		width = 640,
		height = 360,
		onptz
	}: {
		mediaKey?: string;
		aspectRatio?: number;
		enabled?: boolean;
		playing?: boolean;
		width?: number;
		height?: number;
		onptz?: () => void;
	} = $props();
</script>

<div style:width={`${width}px`} style:height={`${height}px`}>
	<FocusedMediaViewport {mediaKey} {aspectRatio} {enabled}>
		<div data-media-pixels data-playing={playing} class="relative size-full bg-emerald-800">
			<span data-test-box class="absolute top-1/4 left-1/4 size-1/4 border-2 border-white"></span>
		</div>
	</FocusedMediaViewport>
</div>
<button type="button" onclick={onptz}>Physical PTZ zoom</button>
