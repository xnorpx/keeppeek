<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import EventDetailDrawer from '$lib/components/EventDetailDrawer.svelte';
	import EventResultCard from '$lib/components/EventResultCard.svelte';
	import type { EventBrowserRecord } from '$lib/event-browser';
	import type { CameraListItem, RecordingEvent } from '$lib/types';
	import InfoIcon from '@lucide/svelte/icons/info';
	import SearchIcon from '@lucide/svelte/icons/search';
	import SlidersHorizontalIcon from '@lucide/svelte/icons/sliders-horizontal';

	type Props = {
		state?: 'browse' | 'detail';
	};

	let { state = 'browse' }: Props = $props();
	setCapabilityState();

	const camera = {
		id: 'front-door',
		ip: '192.0.2.41',
		name: 'Front Door',
		manufacturer: 'Reolink',
		model: 'RLC-811A',
		firmware_version: null,
		is_reolink: true,
		profiles: []
	} satisfies CameraListItem;
	const eventSeconds = [
		23_843, 23_462, 22_324, 20_931, 19_277, 17_891, 16_300, 13_929, 11_695, 10_043, 7_398, 6_242,
		4_357, 3_524, 1_266
	];
	const variants = [
		{ kind: 'person', source: 'camera', confidence: 0.94, image: true },
		{ kind: 'person', source: 'camera', confidence: 0.88, image: true },
		{ kind: 'story', source: 'keeppeek', confidence: null, image: true },
		{ kind: 'motion', source: 'camera', confidence: null, image: false },
		{ kind: 'person', source: 'camera', confidence: 0.42, image: true }
	] as const;
	const dayStartMs = Date.parse('2026-08-18T00:00:00Z');
	const records: EventBrowserRecord[] = eventSeconds.map((seconds, index) => {
		const variant = variants[index % variants.length];
		const event = {
			id: `paper-event-${index + 1}`,
			source: variant.source,
			kind: variant.kind,
			start_time_ms: dayStartMs + seconds * 1_000,
			end_time_ms: dayStartMs + seconds * 1_000 + 5_000,
			confidence: variant.confidence,
			bbox: index === 0 ? ([0.3, 0.2, 0.25, 0.5] as const) : null,
			zone: index === 0 ? 'porch' : null,
			thumbnail_url: variant.image ? '/visual-fixtures/event-thumbnail.jpg' : null
		} satisfies RecordingEvent;
		return { camera, event };
	});
	const evidenceRows = [
		[
			'attachments[]',
			'One optional thumbnail URL is returned. Multi-image history is unavailable.'
		],
		[
			'bounding_box',
			'Returned coordinates are drawn over the supplied thumbnail, never re-derived.'
		],
		['payload', 'Not reported by the current event API.'],
		['revision', 'Not reported by the current event API.'],
		[
			'source_id',
			'Only the camera or KeepPeek source category is returned. Publisher identity is unavailable.'
		]
	] as const;
</script>

{#if state === 'browse'}
	<main
		data-paper-scenario="events.desktop.browse"
		class="flex h-[820px] w-[1440px] shrink-0 overflow-hidden rounded-lg border border-hairline bg-ground [font-synthesis:none]"
	>
		<DesktopPaperRail active="events" paperCompact />

		<section
			data-events-browse-main
			class="flex h-[818px] w-[1374px] shrink-0 flex-col"
			aria-label="Events browse"
		>
			<header
				data-events-search-bar
				class="flex h-14 w-[1374px] shrink-0 items-center gap-3 border-b border-hairline px-5"
			>
				<div
					class="flex h-9 w-[1042px] shrink-0 items-center gap-2.5 rounded-sm border border-primary bg-raised px-3"
				>
					<SearchIcon class="size-3.5 shrink-0 text-primary-soft" />
					<div class="flex items-center gap-1.5 font-mono text-[11px] leading-[14px]">
						<span class="rounded-[3px] bg-primary-deep px-2 py-0.5 text-on-primary"
							>camera:front-door</span
						>
						<span class="rounded-[3px] bg-primary-deep px-2 py-0.5 text-on-primary"
							>type:person</span
						>
					</div>
					<span class="font-mono text-xs text-text-faint">confidence:≥0.8</span>
				</div>
				<button
					type="button"
					class="inline-flex h-[34px] w-[102px] shrink-0 items-center gap-1.5 rounded-sm border border-hairline bg-raised px-3 text-xs"
				>
					<SlidersHorizontalIcon class="size-[13px] text-text-muted" />Filters
					<span
						class="grid size-4 place-items-center rounded-full bg-primary font-mono text-[9px] font-semibold text-on-primary"
						>3</span
					>
				</button>
				<div
					class="flex h-[33px] w-[166px] shrink-0 items-center gap-0.5 rounded-sm bg-raised p-[3px] text-xs"
				>
					<button type="button" class="h-[27px] rounded-[3px] bg-surface px-2.5">Grid</button>
					<button type="button" class="h-[27px] px-2.5 text-text-muted">Summary</button>
					<button type="button" class="h-[27px] px-2.5 text-text-muted">List</button>
				</div>
			</header>

			<div
				data-events-result-bar
				class="flex h-10 w-[1374px] shrink-0 items-center gap-3 border-b border-hairline bg-surface px-5 text-xs"
			>
				<span>15 events · mixed API evidence · 2026-08-18</span>
				<span class="flex-1"></span>
				<span class="font-mono text-[10px] tracking-[0.1em] text-text-faint"
					>URL CARRIES THIS QUERY</span
				>
				<span class="text-primary-soft">Save as view</span>
			</div>

			<div data-events-grid class="flex h-[722px] w-[1374px] shrink-0 flex-col gap-3 p-4">
				{#each [0, 1, 2] as row (row)}
					<div
						data-events-row={row + 1}
						class="flex w-[1342px] shrink-0 gap-3 {row === 0 ? 'h-[218px]' : 'h-[216px]'}"
					>
						{#each records.slice(row * 5, row * 5 + 5) as record, column (record.event.id)}
							<EventResultCard
								{record}
								selected={row === 0 && column === 0}
								tabindex={row === 0 && column === 0 ? 0 : -1}
								paperFrame
							/>
						{/each}
					</div>
				{/each}
			</div>
		</section>
	</main>
{:else}
	<main
		data-paper-scenario="events.desktop.detail"
		class="flex h-[669px] w-[1440px] shrink-0 gap-7 border-t border-hairline bg-ground pt-10 [font-synthesis:none]"
	>
		<EventDetailDrawer record={records[0]} paperFrame onclose={() => {}} />

		<section
			data-event-detail-requirements
			class="flex h-[628px] w-[852px] shrink-0 flex-col gap-[22px]"
			aria-label="Event detail contract"
		>
			<h1 class="h-[34px] shrink-0 text-[28px] leading-[34px] font-semibold">
				What the drawer must carry
			</h1>
			<div class="flex h-[229px] shrink-0 flex-col">
				{#each evidenceRows as evidence, index (evidence[0])}
					<div
						class="flex min-h-[45px] flex-1 gap-5 border-b border-hairline py-[13px] last:border-b-0"
					>
						<code class="w-[130px] shrink-0 font-mono text-[11px] leading-[14px] text-primary-soft"
							>{evidence[0]}</code
						>
						<p class="text-[13px] leading-[19px] text-text-muted">{evidence[1]}</p>
					</div>
				{/each}
			</div>
			<div
				class="flex h-[68px] shrink-0 items-start gap-2.5 rounded-md border border-activity bg-activity/10 p-3.5"
			>
				<InfoIcon class="size-[15px] shrink-0 text-activity" />
				<p class="text-[13px] leading-[19px]">
					Backend gap: persistence stores one mutable event row and one thumbnail. Revisions,
					publisher identities, payloads, and multi-image stories require additional contracts.
				</p>
			</div>
		</section>
	</main>
{/if}
