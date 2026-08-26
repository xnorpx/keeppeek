<script lang="ts">
	import { resolve } from '$app/paths';
	import { capabilityActions } from '$lib/capability-actions';
	import {
		eventHasImage,
		type EventBrowserRecord,
		type EventPreviewState
	} from '$lib/event-browser';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import ImageOffIcon from '@lucide/svelte/icons/image-off';
	import LoaderCircleIcon from '@lucide/svelte/icons/loader-circle';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import XIcon from '@lucide/svelte/icons/x';
	import CapabilityGate from './CapabilityGate.svelte';

	type Props = {
		record: EventBrowserRecord;
		previewState?: EventPreviewState;
		paperFrame?: boolean;
		onclose?: () => void;
		onpreviewretry?: () => void;
	};

	let {
		record,
		previewState = 'idle',
		paperFrame = false,
		onclose,
		onpreviewretry
	}: Props = $props();
	const eventTimeFormatter = new Intl.DateTimeFormat(undefined, {
		year: 'numeric',
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit',
		timeZone: 'UTC',
		timeZoneName: 'short'
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

	function revisionLabel(): string {
		return record.event.revision === undefined ? 'Not reported' : String(record.event.revision);
	}

	function attachmentLabel(): string {
		const count = record.event.attachments?.length ?? 0;
		return count === 0 ? 'Not reported' : `${count} reported`;
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
			REVISION {revisionLabel().toUpperCase()}
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
		{:else if eventHasImage(record.event) && previewState === 'unavailable'}
			<div class="grid justify-items-center gap-2 text-text-faint">
				<ImageOffIcon class="size-5" />
				<span class="font-mono text-2xs tracking-caps">PREVIEW UNAVAILABLE</span>
				{#if onpreviewretry}
					<button
						type="button"
						class="inline-flex h-8 items-center gap-1.5 rounded-sm border border-hairline bg-surface px-2.5 text-xs text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						onclick={onpreviewretry}
					>
						<RefreshCwIcon class="size-3.5" /> Retry preview
					</button>
				{/if}
			</div>
		{:else if eventHasImage(record.event)}
			<div class="grid justify-items-center gap-2 text-text-faint">
				<LoaderCircleIcon class="size-5 animate-spin" />
				<span class="font-mono text-2xs tracking-caps">
					{previewState === 'loading' ? 'LOADING PREVIEW' : 'PREVIEW QUEUED'}
				</span>
			</div>
		{:else}
			<div
				class="grid size-full place-items-center border border-dashed border-hairline-strong font-mono text-2xs tracking-caps text-text-faint"
			>
				NO IMAGE REPORTED
			</div>
		{/if}
		{#if record.event.bbox}
			<span
				class="pointer-events-none absolute border-2 border-primary"
				style:left={`${record.event.bbox[0] * 100}%`}
				style:top={`${record.event.bbox[1] * 100}%`}
				style:width={`${record.event.bbox[2] * 100}%`}
				style:height={`${record.event.bbox[3] * 100}%`}
			></span>
			{#if record.event.confidence !== null}
				<span
					class="pointer-events-none absolute rounded-sm bg-primary px-[7px] py-0.5 font-mono text-[10px] font-semibold text-on-primary"
					style:left={`${record.event.bbox[0] * 100}%`}
					style:top={`calc(${record.event.bbox[1] * 100}% - 20px)`}
				>
					{record.event.kind}
					{record.event.confidence.toFixed(2)}
				</span>
			{/if}
		{/if}
		<span class="absolute bottom-3 left-3 font-mono text-[10px] tracking-[0.1em] text-text-faint">
			{record.event.thumbnail_url
				? 'ONE THUMBNAIL URL'
				: eventHasImage(record.event)
					? 'THUMBNAIL REPORTED'
					: 'NO ATTACHMENT'}
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
					<time datetime={new Date(record.event.start_time_ms).toISOString()}>
						{paperFrame
							? paperTimestamp()
							: eventTimeFormatter.format(new Date(record.event.start_time_ms))}
					</time>
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
				<p>attachments[]: {attachmentLabel()}</p>
				<p>revision: {revisionLabel()} · source_id: {record.event.source_id ?? 'Not reported'}</p>
				<p>payload: Not reported by REST API</p>
			</section>
		{:else}
			<section class="h-[127px] shrink-0" aria-label="Event API evidence">
				<div
					class="grid h-full grid-cols-2 gap-px overflow-hidden rounded-md border border-hairline bg-hairline text-xs"
				>
					{#each [['confidence', confidenceLabel()], ['zone', record.event.zone ?? 'Not reported'], ['bounding_box', bboxLabel()], ['attachments[]', attachmentLabel()], ['payload', 'Not reported by REST API'], ['revision', revisionLabel()], ['source_id', record.event.source_id ?? 'Not reported']] as evidence (evidence[0])}
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
