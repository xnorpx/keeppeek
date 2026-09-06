<script lang="ts">
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import VideoTile from './VideoTile.svelte';
	import { sourceKey } from './connection-manager';
	import type { CardSource } from './config';
	import type { CardView } from './elements.svelte';

	let {
		state: view,
		onretry,
		ondemand
	}: { state: CardView; onretry: () => void; ondemand: (sources: CardSource[]) => void } = $props();
	let focus = $state<string | null>(null);
	let config = $derived(view.config);
	let sources = $derived(config?.sources ?? []);
	let focused = $derived(sources.find((source) => sourceKey(source) === focus));
	let visibleSources = $derived(
		config?.layout === 'single' || focused
			? [focused ?? sources[0]].filter((source): source is CardSource => Boolean(source))
			: sources
	);
	$effect(() => ondemand(visibleSources));
</script>

<ha-card class="keeppeek-card" aria-label={config?.title ?? 'KeepPeek cameras'}>
	<header class="card-heading">
		{#if focused && config?.layout !== 'single'}<button
				type="button"
				class="icon-button"
				title="Show all cameras"
				aria-label="Show all cameras"
				onclick={() => {
					focus = null;
				}}><ArrowLeft size={18} /></button
			>{/if}
		<h2>{config?.title ?? 'KeepPeek'}</h2>
		<span class="connection-status"
			>{!view.visible
				? 'Paused'
				: view.snapshot?.status === 'ready'
					? 'Live'
					: view.snapshot?.status === 'error'
						? 'Offline'
						: 'Connecting'}</span
		>
		{#if view.snapshot?.message}<button
				type="button"
				class="icon-button"
				title="Reconnect"
				aria-label="Reconnect"
				onclick={onretry}><RefreshCw size={18} /></button
			>{/if}
	</header>
	{#if view.snapshot?.message}<p class="connection-error" role="alert">
			{view.snapshot.message}
		</p>{/if}
	{#if config?.layout === 'single' && sources.length > 1}
		<label class="camera-picker"
			>Camera<select
				value={sourceKey(visibleSources[0]!)}
				onchange={(event) => {
					focus = event.currentTarget.value;
				}}
				>{#each sources as source (sourceKey(source))}<option value={sourceKey(source)}
						>{source.title ?? source.source_id}</option
					>{/each}</select
			></label
		>
	{/if}
	<div
		class="video-grid"
		style:--columns={Math.min(config?.columns ?? 2, Math.max(1, visibleSources.length))}
	>
		{#each visibleSources as source (sourceKey(source))}
			<VideoTile
				title={source.title ??
					view.snapshot?.cameras.find((camera) => camera.source_id === source.source_id)?.title ??
					source.source_id}
				state={view.visible ? view.snapshot?.streams.get(sourceKey(source)) : undefined}
				ratio={config?.aspect_ratio ?? '16:9'}
				showName={config?.show_name ?? true}
				onfocus={visibleSources.length > 1
					? () => {
							focus = sourceKey(source);
						}
					: undefined}
			/>
		{/each}
	</div>
</ha-card>
