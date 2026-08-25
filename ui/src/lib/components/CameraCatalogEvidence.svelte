<script lang="ts">
	import { firstHttpCameraCatalogSource } from '$lib/camera-wizard';
	import type { CameraCatalogCamera, CameraCatalogInfo } from '$lib/types';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';

	type Props = {
		camera: CameraCatalogCamera;
		catalogInfo?: CameraCatalogInfo | null;
		compact?: boolean;
	};

	let { camera, catalogInfo = null, compact = false }: Props = $props();
	let facts = $derived(
		[
			camera.camera_type,
			camera.resolution_label,
			camera.megapixels === null ? null : `${camera.megapixels} MP`,
			camera.sensor,
			camera.release_year === null ? null : `Released ${camera.release_year}`
		].filter((value): value is string => Boolean(value))
	);
	let physicalFacts = $derived(
		[
			camera.field_of_view && `Field of view ${camera.field_of_view}`,
			camera.night_vision && `Night vision ${camera.night_vision}`,
			[camera.ip_rating, camera.ik_rating].filter(Boolean).join(' · ') || null,
			camera.two_way_audio === null
				? null
				: camera.two_way_audio
					? 'Two-way audio'
					: 'No two-way audio reported',
			camera.community_notes_count > 0
				? `${camera.community_notes_count} community note${camera.community_notes_count === 1 ? '' : 's'}`
				: null
		].filter((value): value is string => Boolean(value))
	);
	let protocols = $derived(camera.protocols.map((protocol) => protocol.toUpperCase()).join(' · '));
	let sourceUrl = $derived(catalogInfo?.website_url ?? 'https://www.cctv-database.com/');
	let modelSourceUrl = $derived(firstHttpCameraCatalogSource(camera.sources));
</script>

<section
	data-camera-catalog-evidence
	class="rounded-sm border border-primary/40 bg-primary/5 px-3 py-3 {compact
		? 'space-y-2'
		: 'space-y-3'}"
	aria-label="Camera catalog reference"
>
	<div class="flex items-center justify-between gap-3">
		<span
			class="inline-flex items-center gap-1.5 font-mono text-2xs tracking-caps text-primary-soft"
		>
			<DatabaseIcon class="size-3" /> MODEL REFERENCE
		</span>
		{#if catalogInfo}
			<span class="shrink-0 font-mono text-2xs text-text-faint">v{catalogInfo.version}</span>
		{/if}
	</div>
	<div class="flex items-start justify-between gap-3">
		<div class="min-w-0">
			<p class="truncate text-sm font-semibold text-foreground">{camera.brand} {camera.model}</p>
			{#if facts.length > 0}<p class="mt-1 text-xs leading-5 text-text-muted">
					{facts.join(' · ')}
				</p>{/if}
		</div>
		<span class="shrink-0 font-mono text-2xs text-activity">NOT PROBED</span>
	</div>
	{#if !compact}
		<dl class="grid grid-cols-2 divide-x divide-hairline border-y border-hairline text-xs">
			<div class="min-w-0 py-2 pr-3">
				<dt class="font-mono text-2xs tracking-caps text-text-faint">PROTOCOLS</dt>
				<dd class="mt-1 truncate font-mono text-text-muted">{protocols || 'Not declared'}</dd>
			</div>
			<div class="min-w-0 py-2 pl-3">
				<dt class="font-mono text-2xs tracking-caps text-text-faint">CODECS</dt>
				<dd class="mt-1 truncate font-mono text-text-muted">
					{camera.codecs.join(' · ') || 'Not declared'}
				</dd>
			</div>
		</dl>
		{#if physicalFacts.length > 0}
			<p class="text-xs leading-5 text-text-muted">{physicalFacts.join(' · ')}</p>
		{/if}
		{#if camera.streams.length > 0}
			<div>
				<p class="font-mono text-2xs tracking-caps text-text-faint">DECLARED STREAMS</p>
				<ul class="mt-1 divide-y divide-hairline border-y border-hairline">
					{#each camera.streams as stream (`${stream.name}-${stream.resolution}-${stream.codec}`)}
						<li class="flex min-h-7 items-center justify-between gap-3 py-1 text-xs">
							<span class="font-medium">{stream.name}</span>
							<span class="text-right font-mono text-2xs text-text-muted">
								{[stream.resolution, stream.fps === null ? null : `${stream.fps} fps`, stream.codec]
									.filter(Boolean)
									.join(' · ') || 'No format declared'}
							</span>
						</li>
					{/each}
				</ul>
			</div>
		{/if}
	{/if}
	<div class="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
		<span class="font-mono text-2xs text-text-faint">REFERENCE ONLY · NO CREDENTIALS</span>
		<span class="flex shrink-0 items-center gap-3">
			{#if modelSourceUrl}
				<a
					href={modelSourceUrl}
					target="_blank"
					rel="noreferrer"
					data-camera-catalog-model-source
					aria-label={`Open source for ${camera.brand} ${camera.model}`}
					class="inline-flex items-center gap-1 text-xs font-medium text-primary-soft underline-offset-2 hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					>Open source <ExternalLinkIcon class="size-3" /></a
				>
			{/if}
			<a
				href={sourceUrl}
				target="_blank"
				rel="noreferrer"
				data-camera-catalog-source
				aria-label="Open CCTV Database catalog"
				class="inline-flex items-center gap-1 text-xs font-medium text-primary-soft underline-offset-2 hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				>CCTV Database <ExternalLinkIcon class="size-3" /></a
			>
		</span>
	</div>
</section>
