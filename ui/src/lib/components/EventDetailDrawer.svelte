<script lang="ts">
	import { resolve } from '$app/paths';
	import { capabilityActions } from '$lib/capability-actions';
	import type { EventBrowserRecord } from '$lib/event-browser';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import XIcon from '@lucide/svelte/icons/x';
	import CapabilityGate from './CapabilityGate.svelte';

	type Props = {
		record: EventBrowserRecord;
		paperFrame?: boolean;
		onclose?: () => void;
	};

	let { record, paperFrame = false, onclose }: Props = $props();
	const eventTimeFormatter = new Intl.DateTimeFormat(undefined, {
		year: 'numeric',
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});

	function cameraLabel(): string {
		return record.camera.name ?? record.camera.id;
	}

	function eventLabel(): string {
		const label = record.event.kind.replaceAll(/[-_]/g, ' ').trim();
		return label ? label.charAt(0).toUpperCase() + label.slice(1) : 'Event';
	}

	function sourceLabel(): string {
		return record.event.source === 'camera' ? 'Camera event source' : 'KeepPeek event pipeline';
	}

	function confidenceLabel(): string {
		return record.event.confidence === null ? 'Not reported' : record.event.confidence.toFixed(2);
	}

	function bboxLabel(): string {
		return record.event.bbox === null
			? 'Not reported'
			: record.event.bbox.map((value) => value.toFixed(3)).join(', ');
	}

	function paperTimestamp(): string {
		return new Date(record.event.start_time_ms).toISOString().slice(11, 23);
	}

	function recordKey(): string {
		return `${encodeURIComponent(record.camera.id)}:${encodeURIComponent(record.event.id)}`;
	}

	function keepHref(): string {
		const date = new Date(record.event.start_time_ms).toISOString().slice(0, 10);
		const search = new URLSearchParams({
			camera: record.camera.id,
			stream: 'main',
			date,
			at: String(record.event.start_time_ms)
		});
		return `${resolve('/keep')}?${search}`;
	}
</script>

<aside
	data-event-detail={recordKey()}
	class="z-[90] flex flex-col overflow-hidden border border-hairline-strong bg-surface {paperFrame
		? 'h-[628px] w-[560px] shrink-0 rounded-lg'
		: 'fixed inset-y-0 right-0 w-full max-w-[35rem] overflow-y-auto border-y-0 border-r-0 shadow-2xl'}"
	aria-label="Event detail"
>
	<header
		class="z-20 flex shrink-0 items-center gap-3 border-b border-hairline bg-surface px-4 {paperFrame
			? 'h-[47px]'
			: 'sticky top-0 min-h-14'}"
	>
		<div class="min-w-0 flex-1">
			<h2 class="truncate text-sm font-semibold">{eventLabel()}</h2>
			{#if !paperFrame}<p class="font-mono text-2xs text-text-faint">
					EVENT ID · {record.event.id}
				</p>{/if}
		</div>
		<span class="font-mono text-[10px] tracking-[0.08em] text-text-faint">
			REVISION NOT REPORTED
		</span>
		<button
			type="button"
			class="grid size-8 place-items-center rounded-sm text-text-muted hover:bg-raised hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			aria-label="Close event detail"
			onclick={() => onclose?.()}
		>
			<XIcon class="size-4" />
		</button>
	</header>

	<div
		class="relative grid shrink-0 place-items-center overflow-hidden bg-video {paperFrame
			? 'h-[250px]'
			: 'aspect-video'}"
	>
		{#if record.event.thumbnail_url}
			{#if paperFrame}
				<span class="font-mono text-[10px] tracking-[0.12em] text-text-faint">
					ONE THUMBNAIL URL
				</span>
			{:else}
				<img src={record.event.thumbnail_url} alt="" class="size-full object-contain" />
			{/if}
		{:else}
			<div
				class="grid size-full place-items-center border border-dashed border-hairline-strong font-mono text-2xs tracking-caps text-text-faint"
			>
				NO IMAGE REPORTED
			</div>
		{/if}
		{#if record.event.bbox}
			<span
				class="absolute border-2 border-primary"
				style:left={`${record.event.bbox[0] * 100}%`}
				style:top={`${record.event.bbox[1] * 100}%`}
				style:width={`${record.event.bbox[2] * 100}%`}
				style:height={`${record.event.bbox[3] * 100}%`}
			></span>
			{#if record.event.confidence !== null}
				<span
					class="absolute rounded-sm bg-primary px-[7px] py-0.5 font-mono text-[10px] font-semibold text-on-primary"
					style:left={`${record.event.bbox[0] * 100}%`}
					style:top={`calc(${record.event.bbox[1] * 100}% - 20px)`}
				>
					{record.event.kind}
					{record.event.confidence.toFixed(2)}
				</span>
			{/if}
		{/if}
		<span class="absolute bottom-3 left-3 font-mono text-[10px] tracking-[0.1em] text-text-faint">
			{record.event.thumbnail_url ? 'ONE THUMBNAIL URL' : 'NO ATTACHMENT'}
		</span>
	</div>

	<div class="flex h-[65px] shrink-0 gap-1.5 border-b border-hairline px-4 py-2.5">
		{#each Array.from({ length: 4 }) as _, index (index)}
			<div
				class="h-11 min-w-0 flex-1 rounded-sm border {index === 0 && record.event.thumbnail_url
					? 'border-primary bg-video'
					: 'border-hairline bg-ground'}"
			></div>
		{/each}
	</div>

	<div class="flex flex-col gap-3 p-4 {paperFrame ? 'h-[264px] shrink-0' : ''}">
		<dl class="flex h-[31px] shrink-0 gap-5 text-xs">
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">WHEN</dt>
				<dd class="font-mono">
					{paperFrame
						? paperTimestamp()
						: eventTimeFormatter.format(new Date(record.event.start_time_ms))}
				</dd>
			</div>
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">CAMERA</dt>
				<dd>{cameraLabel()}</dd>
			</div>
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">REPORTED BY</dt>
				<dd>{sourceLabel()}</dd>
			</div>
		</dl>

		{#if paperFrame}
			<section
				class="h-[127px] shrink-0 rounded-md bg-raised p-3 font-mono text-[11px] leading-[17px] text-text-muted"
				aria-label="Event API evidence"
			>
				<p class="font-mono text-[10px] leading-3 tracking-caps text-text-faint">
					REST EVENT EVIDENCE
				</p>
				<p>confidence: {confidenceLabel()} · zone: {record.event.zone ?? 'Not reported'}</p>
				<p>bounding_box: {bboxLabel()}</p>
				<p>attachments[]: {record.event.thumbnail_url ? 'One thumbnail URL' : 'Not reported'}</p>
				<p>payload · revision · source_id: Not reported by REST API</p>
			</section>
		{:else}
			<section class="h-[127px] shrink-0" aria-label="Event API evidence">
				<div
					class="grid h-full grid-cols-2 gap-px overflow-hidden rounded-md border border-hairline bg-hairline text-xs"
				>
					{#each [['confidence', confidenceLabel()], ['zone', record.event.zone ?? 'Not reported'], ['bounding_box', bboxLabel()], ['attachments[]', record.event.thumbnail_url ? 'One thumbnail URL' : 'Not reported'], ['payload', 'Not reported by REST API'], ['revision', 'Not reported by REST API'], ['source_id', 'Not reported by REST API']] as evidence (evidence[0])}
						<div class="bg-surface px-3 py-2">
							<p class="font-mono text-[10px] tracking-caps text-primary-soft">{evidence[0]}</p>
							<p class="mt-0.5 truncate text-text-muted">{evidence[1]}</p>
						</div>
					{/each}
				</div>
			</section>
		{/if}

		<div class="flex h-[50px] shrink-0 items-center gap-2.5 overflow-hidden">
			<a
				href={keepHref()}
				class="inline-flex h-9 shrink-0 items-center gap-1.5 rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			>
				<ExternalLinkIcon class="size-3.5" />Open at this moment
			</a>
			<CapabilityGate {...capabilityActions.exportMoment} />
			<CapabilityGate {...capabilityActions.bookmarkMoment} />
		</div>
	</div>
</aside>
