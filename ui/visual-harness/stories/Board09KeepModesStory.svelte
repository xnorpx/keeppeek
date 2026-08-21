<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import KeepExportPanel from '$lib/components/KeepExportPanel.svelte';
	import KeepStories from '$lib/components/KeepStories.svelte';
	import KeepSwimlanes from '$lib/components/KeepSwimlanes.svelte';
	import { setControlClient } from '$lib/control-context';
	import { ControlClient } from '$lib/control-client';
	import type { CameraListItem, RecordingEvent, RecordingSegment } from '$lib/types';

	type Props = {
		state?: 'stories' | 'calendar' | 'export' | 'swimlanes';
	};

	let { state = 'stories' }: Props = $props();
	setControlClient(new ControlClient());
	setCapabilityState();

	const selectedDate = '2026-08-18';
	const dayStartMs = Date.parse(`${selectedDate}T00:00:00Z`);
	const at = (time: string) => Date.parse(`${selectedDate}T${time}Z`);
	const dates = [
		'2026-08-18',
		'2026-08-17',
		'2026-08-16',
		'2026-08-15',
		'2026-08-13',
		'2026-08-12',
		'2026-08-11'
	];
	const stories: RecordingEvent[] = [
		{
			id: 'story-paper-1',
			source: 'camera',
			kind: 'story',
			start_time_ms: at('06:12:00'),
			end_time_ms: at('06:14:00'),
			confidence: null,
			bbox: null,
			zone: null,
			thumbnail_url: '/visual-fixtures/story-thumbnail.jpg'
		}
	];
	const exportSegment: RecordingSegment = {
		stream: 'main',
		date: selectedDate,
		hour: '06',
		filename: 'front-door.mp4',
		url: '/story/keep/front-door.mp4',
		start_time_ms: at('06:11:48'),
		end_time_ms: at('06:14:20'),
		duration_ms: 152_000
	};
	const cameraNames = [
		['front-door', 'Front Door'],
		['driveway', 'Driveway'],
		['side-gate', 'Side Gate'],
		['back-yard', 'Back Yard']
	] as const;
	const cameras: CameraListItem[] = cameraNames.map(([id, name], index) => ({
		id,
		ip: `192.0.2.${index + 51}`,
		name,
		manufacturer: 'ONVIF',
		model: `Camera ${index + 1}`,
		firmware_version: null,
		is_reolink: false,
		profiles: []
	}));

	function segment(cameraId: string, start: string, end: string): RecordingSegment {
		return {
			stream: 'main',
			date: selectedDate,
			hour: '06',
			filename: `${cameraId}-${start}.mp4`,
			url: `/story/keep/${cameraId}-${start}.mp4`,
			start_time_ms: at(start),
			end_time_ms: at(end),
			duration_ms: at(end) - at(start)
		};
	}

	function event(id: string, kind: string, start: string, end: string | null): RecordingEvent {
		return {
			id,
			source: 'camera',
			kind,
			start_time_ms: at(start),
			end_time_ms: end === null ? null : at(end),
			confidence: null,
			bbox: null,
			zone: null,
			thumbnail_url: null
		};
	}

	const lanes = [
		{
			camera: cameras[0],
			segments: [segment('front-door', '06:00:00', '07:00:00')],
			events: [
				event('front-person', 'person', '06:13:12', '06:18:36'),
				event('front-motion', 'motion', '06:28:48', '06:31:48')
			]
		},
		{
			camera: cameras[1],
			segments: [segment('driveway', '06:00:00', '07:00:00')],
			events: [event('driveway-person', 'person', '06:12:00', '06:19:12')]
		},
		{
			camera: cameras[2],
			segments: [segment('side-gate', '06:00:00', '07:00:00')],
			events: [event('side-motion', 'motion', '06:36:00', '06:39:36')]
		},
		{
			camera: cameras[3],
			segments: [
				segment('back-yard', '06:00:00', '06:21:36'),
				segment('back-yard', '06:30:00', '07:00:00')
			],
			events: []
		}
	];
</script>

{#if state === 'stories'}
	<main
		data-paper-scenario="keep.desktop.stories"
		class="h-[413px] w-[467px] shrink-0 [font-synthesis:none]"
	>
		<KeepStories
			events={stories}
			{dates}
			{selectedDate}
			ondate={() => {}}
			onseek={() => {}}
			panel="stories"
			paperFrame
		/>
	</main>
{:else if state === 'calendar'}
	<main
		data-paper-scenario="keep.desktop.calendar"
		class="h-[413px] w-[467px] shrink-0 [font-synthesis:none]"
	>
		<KeepStories
			events={stories}
			{dates}
			{selectedDate}
			ondate={() => {}}
			onseek={() => {}}
			panel="calendar"
			paperFrame
		/>
	</main>
{:else if state === 'export'}
	<main
		data-paper-scenario="keep.desktop.export-gated"
		class="h-[413px] w-[467px] shrink-0 [font-synthesis:none]"
	>
		<KeepExportPanel
			sourceId="front-door"
			sourceName="Front Door"
			segment={exportSegment}
			bitrateKbps={6_200}
			paperFrame
		/>
	</main>
{:else}
	<main
		data-paper-scenario="keep.desktop.swimlanes"
		class="h-[363px] w-[1440px] shrink-0 [font-synthesis:none]"
	>
		<KeepSwimlanes
			{cameras}
			selectedCameraId="front-door"
			date={selectedDate}
			anchorMs={dayStartMs + 6 * 60 * 60_000 + 59 * 60_000}
			playheadMs={at('06:14:24')}
			{lanes}
			onselect={() => {}}
			paperFrame
		/>
	</main>
{/if}
