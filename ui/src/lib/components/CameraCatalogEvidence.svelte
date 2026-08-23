<script lang="ts">
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
			camera.sensor
		].filter((value): value is string => Boolean(value))
	);
	let streams = $derived(
		camera.streams
			.slice(0, 2)
			.map((stream) =>
				[stream.name, stream.resolution, stream.codec]
					.filter((value): value is string => Boolean(value))
					.join(' ')
			)
			.filter(Boolean)
			.join(' · ')
	);
	let protocols = $derived(camera.protocols.map((protocol) => protocol.toUpperCase()).join(' · '));
	let sourceUrl = $derived(catalogInfo?.website_url ?? 'https://www.cctv-database.com/');
	let modelSourceUrl = $derived(firstExternalSource(camera.sources));

	function firstExternalSource(sources: string[]): string | null {
		for (const source of sources) {
			try {
				const url = new URL(source);
				if (url.protocol === 'https:' || url.protocol === 'http:') return url.href;
			} catch {
				continue;
			}
		}
		return null;
	}
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
				<dt class="font-mono text-2xs tracking-caps text-text-faint">DECLARED STREAMS</dt>
				<dd class="mt-1 truncate font-mono text-text-muted">{streams || 'Not declared'}</dd>
			</div>
		</dl>
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
