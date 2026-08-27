<script lang="ts">
	import { canonicalEventAttachment, eventOwnsCanonicalBoundingBox } from '$lib/event-presentation';
	import type { EventPreviewState } from '$lib/event-browser';
	import type { RecordingEvent } from '$lib/types';
	import ImageOffIcon from '@lucide/svelte/icons/image-off';
	import LoaderCircleIcon from '@lucide/svelte/icons/loader-circle';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import EventIcon from './EventIcon.svelte';

	type Props = {
		event: RecordingEvent;
		cameraLabel: string;
		previewState?: EventPreviewState;
		fit?: 'cover' | 'contain';
		showBoundingBox?: boolean;
		onretry?: () => void;
	};

	let {
		event,
		cameraLabel,
		previewState = 'idle',
		fit = 'cover',
		showBoundingBox = false,
		onretry
	}: Props = $props();
	let canonicalAttachment = $derived(
		canonicalEventAttachment(event.attachments ?? [], event.canonical_attachment_id)
	);
	let hasImage = $derived(event.thumbnail_url !== null || canonicalAttachment !== null);
	let unavailable = $derived(
		previewState === 'unavailable' || event.image_availability === 'unavailable'
	);
	let eventLabel = $derived.by(() => {
		const label = event.kind.replaceAll(/[-_]/g, ' ').trim();
		return label ? label.charAt(0).toUpperCase() + label.slice(1) : 'Event';
	});
</script>

{#if event.thumbnail_url}
	<img
		data-event-preview-image
		src={event.thumbnail_url}
		alt={`${eventLabel} from ${cameraLabel}`}
		loading="lazy"
		decoding="async"
		class="size-full {fit === 'contain' ? 'object-contain' : 'object-cover'}"
	/>
	{#if showBoundingBox && eventOwnsCanonicalBoundingBox(event) && event.bbox}
		<span
			data-event-bounding-box
			class="pointer-events-none absolute border-2 border-primary"
			style:left={`${event.bbox[0] * 100}%`}
			style:top={`${event.bbox[1] * 100}%`}
			style:width={`${event.bbox[2] * 100}%`}
			style:height={`${event.bbox[3] * 100}%`}
		></span>
		{#if event.confidence !== null}
			<span
				class="pointer-events-none absolute rounded-sm bg-primary px-[7px] py-0.5 font-mono text-[10px] font-semibold text-on-primary"
				style:left={`${event.bbox[0] * 100}%`}
				style:top={`calc(${event.bbox[1] * 100}% - 20px)`}
			>
				{event.kind}
				{event.confidence.toFixed(2)}
			</span>
		{/if}
	{/if}
{:else if !hasImage}
	<div
		data-event-preview-state="none"
		class="grid size-full place-items-center border border-dashed border-hairline-strong text-text-faint"
		aria-label={`${eventLabel} from ${cameraLabel} has no image`}
	>
		<span class="grid justify-items-center gap-1.5 font-mono text-2xs tracking-caps">
			<EventIcon iconKey={event.icon_key} eventType={event.kind} class="size-4" />
			NO IMAGE
		</span>
	</div>
{:else if unavailable}
	<div
		data-event-preview-state="unavailable"
		class="grid size-full place-items-center border border-dashed border-hairline-strong text-text-faint"
		aria-label="Event image unavailable"
		aria-live="polite"
	>
		<span class="grid justify-items-center gap-1.5 font-mono text-2xs tracking-caps">
			<ImageOffIcon class="size-4" aria-hidden="true" />
			<span>PREVIEW UNAVAILABLE</span>
			{#if onretry}
				<button
					type="button"
					class="inline-flex h-8 items-center gap-1.5 rounded-sm border border-hairline bg-surface px-2.5 font-sans text-xs tracking-normal text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					onclick={onretry}
				>
					<RefreshCwIcon class="size-3.5" aria-hidden="true" /> Retry preview
				</button>
			{/if}
		</span>
	</div>
{:else if previewState === 'loading'}
	<div class="grid size-full place-items-center bg-raised text-text-faint" aria-live="polite">
		<span class="sr-only">Loading image for {eventLabel} from {cameraLabel}</span>
		<LoaderCircleIcon class="size-4 animate-spin" aria-hidden="true" />
	</div>
{:else}
	<div
		class="grid size-full place-items-center bg-video text-text-faint"
		aria-label={`${eventLabel} image ${previewState === 'queued' ? 'queued' : 'available'}`}
	>
		<EventIcon iconKey={event.icon_key} eventType={event.kind} class="size-4" />
	</div>
{/if}
