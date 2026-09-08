<script lang="ts">
	import type { Snippet } from 'svelte';
	import { createFocusedMediaControls } from '$lib/focused-media-gestures';
	import { DIGITAL_ZOOM_MAX, DIGITAL_ZOOM_MIN, fitMediaSize } from '$lib/focused-media-geometry';
	import ZoomInIcon from '@lucide/svelte/icons/zoom-in';
	import ZoomOutIcon from '@lucide/svelte/icons/zoom-out';
	import ScanIcon from '@lucide/svelte/icons/scan';

	type Props = {
		children: Snippet;
		toolbar?: Snippet;
		mediaKey: string;
		aspectRatio?: number;
		enabled?: boolean;
		controlsPosition?: 'top-left' | 'bottom-left';
		onzoomchange?: (scale: number) => void;
	};

	let {
		children,
		toolbar,
		mediaKey,
		aspectRatio = 16 / 9,
		enabled = true,
		controlsPosition = 'top-left',
		onzoomchange
	}: Props = $props();
	const zoomId = $props.id();
	let width = $state(0);
	let height = $state(0);
	let scale = $state(DIGITAL_ZOOM_MIN);
	let controls = $state.raw<ReturnType<typeof createFocusedMediaControls> | null>(null);
	let fitted = $derived(fitMediaSize({ width, height }, { width: aspectRatio, height: 1 }));

	function attachLayer(layer: HTMLDivElement) {
		const viewport = layer.parentElement;
		if (!viewport) throw new Error('A focused media layer requires a viewport');
		scale = DIGITAL_ZOOM_MIN;
		const instance = createFocusedMediaControls(viewport, layer, (value) => {
			scale = value;
			onzoomchange?.(value);
		});
		controls = instance;
		const observer = new ResizeObserver(instance.refresh);
		observer.observe(viewport);
		let previousMediaKey: string | undefined;
		$effect(() => {
			if (mediaKey === previousMediaKey) return;
			previousMediaKey = mediaKey;
			instance.reset();
		});
		return () => {
			observer.disconnect();
			instance.destroy();
			controls = null;
		};
	}
</script>

{#if enabled}
	<div
		data-focused-media
		data-digital-zoom={scale}
		class="relative flex size-full min-h-0 flex-col overflow-hidden"
	>
		<div
			data-focused-media-content
			class="grid min-h-0 flex-1 place-items-center overflow-hidden"
			bind:clientWidth={width}
			bind:clientHeight={height}
		>
			<div
				role="application"
				aria-label="Digital zoom viewport"
				aria-describedby={zoomId}
				aria-keyshortcuts="0 ArrowLeft ArrowRight ArrowUp ArrowDown Escape"
				tabindex="-1"
				class="relative shrink-0 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
				style:width={fitted.width > 0 ? `${fitted.width}px` : '100%'}
				style:height={fitted.height > 0 ? `${fitted.height}px` : '100%'}
			>
				<div data-focused-media-layer class="relative size-full" {@attach attachLayer}>
					{@render children()}
				</div>
			</div>
		</div>
		<div
			data-focused-media-toolbar
			class="flex shrink-0 items-center gap-2 overflow-x-auto overflow-y-hidden {controlsPosition ===
			'top-left'
				? 'order-first'
				: ''}"
			style:min-height="var(--focused-media-toolbar-height, 4.375rem)"
		>
			<div
				data-digital-zoom-controls
				role="group"
				aria-label="Digital zoom controls"
				class="relative z-30 m-2 flex shrink-0 items-center gap-1 rounded-sm border border-hairline-strong bg-surface/95 text-foreground shadow-sm"
				style:padding="var(--focused-media-control-padding, 0.25rem)"
			>
				<div class="flex min-w-14 items-center gap-1 px-1">
					<span class="text-[10px] font-medium text-text-muted">Digital</span>
					<span
						id={zoomId}
						role="meter"
						aria-label="Digital zoom level"
						aria-valuemin={DIGITAL_ZOOM_MIN}
						aria-valuemax={DIGITAL_ZOOM_MAX}
						aria-valuenow={scale}
						aria-valuetext={`${scale.toFixed(1)} times`}
						aria-live="polite"
						class="font-mono text-xs tabular-nums"
					>
						{scale.toFixed(1)}x
					</span>
				</div>
				<button
					type="button"
					aria-label="Digital zoom out"
					title="Digital zoom out (-)"
					class="grid shrink-0 place-items-center rounded-sm hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40"
					style:width="var(--focused-media-control-size, 2.75rem)"
					style:height="var(--focused-media-control-size, 2.75rem)"
					disabled={!controls || scale <= DIGITAL_ZOOM_MIN}
					onclick={() => {
						controls?.zoomOut();
						controls?.focus();
					}}
				>
					<ZoomOutIcon class="size-4" aria-hidden="true" />
				</button>
				<button
					type="button"
					aria-label="Digital zoom in"
					title="Digital zoom in (+)"
					class="grid shrink-0 place-items-center rounded-sm hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40"
					style:width="var(--focused-media-control-size, 2.75rem)"
					style:height="var(--focused-media-control-size, 2.75rem)"
					disabled={!controls || scale >= DIGITAL_ZOOM_MAX}
					onclick={() => {
						controls?.zoomIn();
						controls?.focus();
					}}
				>
					<ZoomInIcon class="size-4" aria-hidden="true" />
				</button>
				<button
					type="button"
					aria-label="Reset digital zoom"
					title="Reset digital zoom (0)"
					class="grid shrink-0 place-items-center rounded-sm hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40"
					style:width="var(--focused-media-control-size, 2.75rem)"
					style:height="var(--focused-media-control-size, 2.75rem)"
					disabled={!controls || scale <= DIGITAL_ZOOM_MIN}
					onclick={() => {
						controls?.reset();
						controls?.focus();
					}}
				>
					<ScanIcon class="size-4" aria-hidden="true" />
				</button>
			</div>
			{@render toolbar?.()}
		</div>
	</div>
{:else}
	{@render children()}
{/if}
