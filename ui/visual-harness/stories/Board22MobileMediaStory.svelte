<script lang="ts">
	import EventResultCard from '$lib/components/EventResultCard.svelte';
	import MobileNavigation from '$lib/components/MobileNavigation.svelte';
	import PeekCameraTile from '$lib/components/PeekCameraTile.svelte';
	import VerticalTimeline from '$lib/components/VerticalTimeline.svelte';
	import type { EventBrowserRecord } from '$lib/event-browser';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type { CameraHealth, CameraListItem, RecordingEvent, RecordingSegment } from '$lib/types';
	import PlayIcon from '@lucide/svelte/icons/play';
	import SearchIcon from '@lucide/svelte/icons/search';
	import MobileDeviceStatusBar from './MobileDeviceStatusBar.svelte';

	type State = 'events' | 'keep' | 'peek';
	type Props = { state: State };
	let { state }: Props = $props();
	setLivePeer();

	const nowMs = Date.parse('2026-08-18T06:37:23Z');
	const dayStartMs = Date.parse('2026-08-18T00:00:00Z');
	const names = [
		['front-door', 'Front Door'],
		['driveway', 'Driveway'],
		['porch', 'Porch'],
		['back-yard', 'Back Yard'],
		['yard-ptz', 'Yard PTZ'],
		['side-gate', 'Side Gate'],
		['workshop', 'Workshop']
	] as const;

	function camera(id: string, name: string): CameraListItem {
		return {
			id,
			ip: `192.0.2.${id.length}`,
			name,
			manufacturer: null,
			model: null,
			firmware_version: null,
			is_reolink: false,
			capabilities: {
				ptz: id === 'yard-ptz',
				audio: false,
				events: true,
				recording: true,
				analytics: false,
				imaging: false,
				two_way_audio: false
			},
			profiles: []
		};
	}

	function health(
		id: string,
		name: string,
		cameraState: CameraHealth['state'],
		drops = 0
	): CameraHealth {
		return {
			id,
			ip: `192.0.2.${id.length}`,
			name,
			manufacturer: null,
			model: null,
			firmware_version: null,
			state: cameraState,
			lifecycle: cameraState === 'offline' ? 'Stopped' : 'Connected',
			last_error: cameraState === 'offline' ? 'No footage for 2h 14m' : null,
			configured_profiles: [],
			streams: [
				{
					type: 'sub',
					fps: cameraState === 'degraded' ? 11 : cameraState === 'offline' ? 0 : 15,
					frames: cameraState === 'offline' ? 0 : 1_000,
					drops,
					updated_at_ms: nowMs,
					report_age_ms: cameraState === 'offline' ? 8_040_000 : 20
				}
			]
		};
	}

	const cameras = names.map(([id, name]) => camera(id, name));
	const healthById = new Map<string, CameraHealth>([
		['front-door', health('front-door', 'Front Door', 'healthy')],
		['driveway', health('driveway', 'Driveway', 'healthy')],
		['porch', health('porch', 'Porch', 'degraded', 14)],
		['back-yard', health('back-yard', 'Back Yard', 'offline')],
		['yard-ptz', health('yard-ptz', 'Yard PTZ', 'healthy')],
		['side-gate', health('side-gate', 'Side Gate', 'healthy')],
		['workshop', health('workshop', 'Workshop', 'healthy')]
	]);

	const segments: RecordingSegment[] = [
		{
			stream: 'main',
			date: '2026-08-18',
			hour: '06',
			filename: 'front-door-1.mp4',
			url: '/story/front-door-1.mp4',
			start_time_ms: dayStartMs + 5 * 60 * 60_000 + 20 * 60_000,
			end_time_ms: dayStartMs + 5 * 60 * 60_000 + 55 * 60_000,
			duration_ms: 35 * 60_000
		},
		{
			stream: 'main',
			date: '2026-08-18',
			hour: '06',
			filename: 'front-door-2.mp4',
			url: '/story/front-door-2.mp4',
			start_time_ms: dayStartMs + 6 * 60 * 60_000,
			end_time_ms: nowMs,
			duration_ms: 37 * 60_000 + 23_000
		}
	];

	function event(
		id: string,
		kind: string,
		minutesAgo: number,
		options: Partial<RecordingEvent> = {}
	): RecordingEvent {
		return {
			id,
			source: 'camera',
			kind,
			start_time_ms: nowMs - minutesAgo * 60_000,
			end_time_ms: null,
			confidence: null,
			bbox: null,
			zone: null,
			thumbnail_url: null,
			...options
		};
	}

	const timelineEvents = [
		event('person', 'person at front step', 0, { confidence: 0.91 }),
		event('delivery', 'delivery van stopped', 25),
		event('motion', 'motion', 48),
		event('vehicle', 'car turned in the drive', 64)
	];
	const eventRecords: EventBrowserRecord[] = [
		{
			camera: cameras[0],
			event: event('person-high', 'person', 0, { confidence: 0.91, zone: 'porch' })
		},
		{ camera: cameras[1], event: event('vehicle', 'vehicle', 25, { confidence: 0.88 }) },
		{ camera: cameras[2], event: event('motion', 'motion', 49) },
		{ camera: cameras[4], event: event('animal-story', 'story', 95, { source: 'keeppeek' }) }
	];
	const scenarioIds: Record<State, string> = {
		peek: 'peek.mobile.live',
		keep: 'keep.mobile.timeline',
		events: 'events.mobile.browse'
	};
</script>

<main
	data-paper-scenario={scenarioIds[state]}
	class="flex h-[844px] w-[390px] flex-col overflow-hidden rounded-lg border border-hairline-strong bg-ground [font-synthesis:none]"
>
	<MobileDeviceStatusBar />
	{#if state === 'peek'}
		<header class="flex h-[50px] shrink-0 items-center justify-between px-4">
			<div class="flex items-baseline gap-2">
				<h1 class="text-2xl leading-[34px] font-semibold">Peek</h1>
				<span class="font-mono text-2xs leading-[14px] text-text-faint">6 LIVE</span>
			</div>
			<span
				class="rounded-full border border-hairline-strong px-3 py-1.5 text-xs leading-4 text-text-muted"
				>Front of house</span
			>
		</header>
		<div class="flex h-[652px] shrink-0 flex-col gap-2 p-[15px]">
			<PeekCameraTile
				camera={cameras[0]}
				health={healthById.get(cameras[0].id)}
				stream="sub"
				mobileFeatured
				onfocus={() => {}}
			/>
			<div class="grid w-[356px] grid-cols-[174px_174px] gap-2">
				{#each cameras.slice(1) as current (current.id)}
					<PeekCameraTile
						camera={current}
						health={healthById.get(current.id)}
						stream="sub"
						onfocus={() => {}}
					/>
				{/each}
			</div>
		</div>
		<MobileNavigation pathname="/" fixed={false} />
	{:else if state === 'keep'}
		<header class="flex h-[50px] shrink-0 items-center justify-between px-4">
			<div class="flex items-baseline gap-2">
				<h1 class="text-2xl leading-[34px] font-semibold">Keep</h1>
				<span class="font-mono text-2xs leading-[14px] text-text-faint">MON 18 AUG</span>
			</div>
			<span
				class="rounded-full border border-hairline-strong px-3 py-1.5 text-xs leading-4 text-text-muted"
				>Front Door</span
			>
		</header>
		<section class="relative h-[220px] shrink-0 bg-video" aria-label="Recorded video player">
			<div
				class="absolute inset-x-2.5 top-2.5 flex justify-between font-mono text-2xs leading-3 text-white/60"
			>
				<span>06:37:23</span><span>1×</span>
			</div>
			<div class="absolute inset-x-2.5 bottom-3 flex items-center gap-3">
				<PlayIcon class="size-5 text-white" fill="currentColor" /><span
					class="h-[3px] flex-1 rounded-full bg-hairline"
					><span class="block h-full w-2/5 bg-primary"></span></span
				>
			</div>
		</section>
		<VerticalTimeline
			{segments}
			events={timelineEvents}
			selectedUrl={segments[1].url}
			playheadMs={nowMs}
			{dayStartMs}
			{nowMs}
			mobileFrame
			onSeek={() => {}}
		/>
		<MobileNavigation pathname="/keep" fixed={false} />
	{:else}
		<header class="flex h-[50px] shrink-0 items-center gap-2 px-4">
			<h1 class="text-2xl leading-[34px] font-semibold">Events</h1>
			<span class="font-mono text-2xs leading-[14px] text-text-faint">5 TODAY</span>
		</header>
		<section class="flex h-[88px] shrink-0 flex-col gap-2 px-[15px]" aria-label="Event filters">
			<label
				class="flex h-[38px] items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-2.5 text-text-faint"
				><SearchIcon class="size-3.5" /><input
					type="search"
					class="min-w-0 flex-1 bg-transparent text-xs outline-none"
					value="camera:front-door type:person"
					aria-label="Search events"
				/></label
			>
			<div class="flex h-7 gap-1.5">
				{#each ['People', 'Vehicles', 'Animals', 'Today'] as filter, index (filter)}<button
						type="button"
						class="rounded-full border px-2.5 text-2xs {index === 0
							? 'border-primary bg-primary text-on-primary'
							: 'border-hairline-strong text-text-muted'}">{filter}</button
					>{/each}
			</div>
		</section>
		<div class="flex h-[564px] shrink-0 flex-col gap-2.5 overflow-hidden p-[15px]">
			{#each eventRecords as record, index (`${record.camera.id}-${record.event.id}`)}
				<EventResultCard {record} mobileVariant={index === 0 ? 'hero' : 'row'} />
			{/each}
		</div>
		<MobileNavigation pathname="/events" fixed={false} />
	{/if}
</main>
