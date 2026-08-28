<script lang="ts">
	import { resolve } from '$app/paths';
	import { capabilityActions } from '$lib/capability-actions';
	import {
		eventHasImage,
		eventKeepSearchParams,
		type EventBrowserRecord,
		type EventPreviewState
	} from '$lib/event-browser';
	import { orderedEventAttachments } from '$lib/event-presentation';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import CheckCircleIcon from '@lucide/svelte/icons/circle-check';
	import XIcon from '@lucide/svelte/icons/x';
	import CapabilityGate from './CapabilityGate.svelte';
	import EventPreview from './EventPreview.svelte';
	import EventIcon from './EventIcon.svelte';

	type Props = {
		record: EventBrowserRecord;
		previewState?: EventPreviewState;
		paperFrame?: boolean;
		returnHref?: string | null;
		alreadyExported?: boolean;
		onclose?: () => void;
		onpreviewretry?: () => void;
	};

	let {
		record,
		previewState = 'idle',
		paperFrame = false,
		returnHref = null,
		alreadyExported = false,
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
	let attachmentSlots = $derived(
		[
			...orderedEventAttachments(record.event).slice(0, 4),
			...Array.from(
				{ length: Math.max(0, 4 - (record.event.attachments?.length ?? 0)) },
				() => null
			)
		].slice(0, 4)
	);
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

	function keepHref(mode?: 'export'): string {
		const search = eventKeepSearchParams(record, mode ?? 'timeline', returnHref);
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
		{#if paperFrame && record.event.thumbnail_url}
			<span class="font-mono text-[10px] tracking-[0.12em] text-text-faint">
				ONE THUMBNAIL URL
			</span>
		{:else}
			<EventPreview
				event={record.event}
				cameraLabel={cameraLabel()}
				{previewState}
				fit="contain"
				showBoundingBox
				onretry={onpreviewretry}
			/>
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
		{#each attachmentSlots as attachment, index (`${attachment?.id ?? 'empty'}:${index}`)}
			<div
				data-event-attachment={attachment?.id ?? undefined}
				title={attachment ? `${attachment.type}, position ${attachment.ordinal + 1}` : undefined}
				class="grid h-11 min-w-0 flex-1 place-items-center overflow-hidden rounded-sm border {attachment?.id ===
				record.event.canonical_attachment_id
					? 'border-primary bg-video'
					: 'border-hairline bg-ground'}"
			>
				{#if !paperFrame && attachment?.id === record.event.canonical_attachment_id && record.event.thumbnail_url}
					<img src={record.event.thumbnail_url} alt="" class="size-full object-cover" />
				{:else if attachment}
					<EventIcon
						iconKey={record.event.icon_key}
						eventType={record.event.kind}
						class="size-3.5 text-text-faint"
					/>
				{/if}
			</div>
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

		{#if alreadyExported}
			<p
				data-event-export-status="ready"
				class="flex h-5 shrink-0 items-center gap-1.5 font-mono text-[10px] tracking-caps text-healthy"
			>
				<CheckCircleIcon class="size-3.5" /> Already exported
			</p>
		{/if}
		<div class="flex h-[50px] shrink-0 items-center gap-2.5 overflow-hidden">
			<a
				href={keepHref()}
				class="inline-flex h-9 shrink-0 items-center gap-1.5 rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			>
				<ExternalLinkIcon class="size-3.5" />Open at this moment
			</a>
			<CapabilityGate {...capabilityActions.exportMoment}>
				<a
					href={keepHref('export')}
					class="inline-flex h-9 shrink-0 items-center gap-1.5 rounded-sm border border-hairline px-3 text-xs font-semibold focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				>
					<ExternalLinkIcon class="size-3.5" /> Export event
				</a>
			</CapabilityGate>
			<CapabilityGate {...capabilityActions.bookmarkMoment} class="min-w-0 flex-1" />
		</div>
	</div>
</aside>
