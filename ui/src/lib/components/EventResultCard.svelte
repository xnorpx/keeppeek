<script lang="ts">
	import { eventHasImage, type EventBrowserRecord } from '$lib/event-browser';
	import { onMount } from 'svelte';

	type Props = {
		record: EventBrowserRecord;
		selected?: boolean;
		mobileVariant?: 'hero' | 'row';
		paperFrame?: boolean;
		tabindex?: number;
		onfocus?: () => void;
		onkeydown?: (event: KeyboardEvent) => void;
		onclick?: () => void;
		onpreviewrequest?: () => void;
	};

	let {
		record,
		selected = false,
		mobileVariant = 'row',
		paperFrame = false,
		tabindex = 0,
		onfocus,
		onkeydown,
		onclick,
		onpreviewrequest
	}: Props = $props();
	let cardElement: HTMLButtonElement | null = null;

	onMount(() => {
		if (
			!cardElement ||
			!onpreviewrequest ||
			record.event.thumbnail_url ||
			!eventHasImage(record.event)
		) {
			return;
		}
		const observer = new IntersectionObserver(
			(entries) => {
				if (!entries.some((entry) => entry.isIntersecting)) return;
				onpreviewrequest();
				observer.disconnect();
			},
			{ rootMargin: '100% 0px' }
		);
		observer.observe(cardElement);
		return () => observer.disconnect();
	});
	const eventTimeFormatter = new Intl.DateTimeFormat(undefined, {
		year: 'numeric',
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});
	const paperTimeFormatter = new Intl.DateTimeFormat('en-GB', {
		timeZone: 'UTC',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit',
		hour12: false
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

	function paperSourceLabel(): string {
		return record.event.source === 'camera' ? 'CAMERA SOURCE' : 'KEEPPEEK PIPELINE';
	}

	function recordKey(): string {
		return `${encodeURIComponent(record.camera.id)}:${encodeURIComponent(record.event.id)}`;
	}
</script>

<button
	bind:this={cardElement}
	type="button"
	data-event-paper-frame={paperFrame || undefined}
	data-event-card={recordKey()}
	class="group min-w-0 overflow-hidden border bg-surface text-left transition-colors hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {paperFrame
		? 'flex h-full flex-1 flex-col gap-2 rounded-lg p-2'
		: mobileVariant === 'hero'
			? 'rounded-md max-md:h-64 max-md:p-0 md:p-2'
			: 'rounded-md max-md:grid max-md:h-[78px] max-md:grid-cols-[86px_minmax(0,1fr)] max-md:items-center max-md:gap-2.5 max-md:p-2.5 md:p-2'} {selected
		? paperFrame
			? 'border-2 border-primary'
			: 'border-primary'
		: 'border-hairline'}"
	aria-label={`Open ${eventLabel()} from ${cameraLabel()}`}
	{tabindex}
	{onfocus}
	{onkeydown}
	{onclick}
>
	<div
		class="relative overflow-hidden rounded-sm {record.event.thumbnail_url || !paperFrame
			? 'bg-video'
			: 'bg-ground'} {paperFrame
			? 'h-[132px] w-full shrink-0'
			: mobileVariant === 'hero'
				? 'max-md:h-[150px] max-md:rounded-b-none md:aspect-video'
				: 'max-md:h-14 max-md:w-[86px] md:aspect-video'}"
	>
		{#if record.event.thumbnail_url}
			{#if !paperFrame}
				<img
					src={record.event.thumbnail_url}
					alt=""
					loading="lazy"
					decoding="async"
					class="size-full object-cover"
				/>
			{/if}
		{:else if !eventHasImage(record.event)}
			<div
				class="grid size-full place-items-center border border-dashed border-hairline-strong font-mono text-2xs tracking-caps text-text-faint"
			>
				NO IMAGE
			</div>
		{:else}
			<div class="size-full animate-pulse bg-raised" aria-label="Loading event image"></div>
		{/if}
		{#if record.event.confidence !== null}
			<span
				class="absolute right-1.5 {paperFrame
					? 'top-1.5'
					: 'bottom-1.5'} rounded-sm bg-video/85 px-1.5 py-0.5 font-mono text-2xs text-white"
				>{record.event.confidence.toFixed(2)}</span
			>
		{/if}
		{#if record.event.kind.toLocaleLowerCase() === 'story'}
			<span
				class="absolute top-1.5 left-1.5 rounded-sm bg-primary px-1.5 py-0.5 font-mono text-2xs font-semibold tracking-caps text-on-primary"
				>STORY</span
			>
		{/if}
	</div>
	{#if paperFrame}
		<time
			class="block h-[14px] w-full shrink-0 font-mono text-[11px] leading-[14px] text-text-muted"
		>
			{paperTimeFormatter.format(new Date(record.event.start_time_ms))}
		</time>
		<p class="h-4 w-full shrink-0 truncate text-[13px] leading-4 font-medium">{eventLabel()}</p>
		<p class="h-3 w-full shrink-0 truncate font-mono text-[10px] leading-3 text-text-faint">
			{paperSourceLabel()}
		</p>
	{:else}
		<div
			class="space-y-1 {mobileVariant === 'hero'
				? 'max-md:h-[104px] max-md:px-3 max-md:py-2.5'
				: 'max-md:min-w-0 max-md:p-0'} md:px-1 md:pt-2 md:pb-1"
		>
			<time class="block font-mono text-2xs text-text-muted">
				{eventTimeFormatter.format(new Date(record.event.start_time_ms))}
			</time>
			<p class="truncate text-sm font-medium">{eventLabel()}</p>
			<p class="truncate text-xs text-text-muted">{cameraLabel()}</p>
			<p class="font-mono text-2xs tracking-caps text-text-faint">{sourceLabel()}</p>
		</div>
	{/if}
</button>
