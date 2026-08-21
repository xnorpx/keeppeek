<script lang="ts">
	import { useControlClient } from '$lib/control-context';
	import RecordingFilmstripPreview from '$lib/components/RecordingFilmstripPreview.svelte';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import type { CameraListItem, RecordingSegment } from '$lib/types';
	import ClapperboardIcon from '@lucide/svelte/icons/clapperboard';

	type Props = {
		cameras: CameraListItem[];
		selectedCameraId: string;
		date: string;
		timestampMs: number | null;
		playing: boolean;
		playbackRate: number;
		onselect: (cameraId: string, timestampMs: number) => void;
	};

	type CameraRecordings = {
		camera: CameraListItem;
		segments: RecordingSegment[];
	};

	type Preview = {
		camera: CameraListItem;
		segment: RecordingSegment | null;
	};

	let { cameras, selectedCameraId, date, timestampMs, playing, playbackRate, onselect }: Props =
		$props();
	const controlClient = useControlClient();
	let recordings = $state.raw<CameraRecordings[]>([]);
	let loading = $state(false);
	let requestVersion = 0;
	let placeholders = $derived(
		Array.from({ length: Math.min(4, Math.max(1, cameras.length - 1)) }, (_, index) => index)
	);
	let previews = $derived(
		recordings
			.filter(({ camera }) => camera.id !== selectedCameraId)
			.map<Preview>(({ camera, segments }) => ({
				camera,
				segment: previewSegment(camera, segments, timestampMs)
			}))
	);

	function preferredStream(camera: CameraListItem): 'main' | 'sub' {
		return (
			camera.profiles.find(
				(profile) => profile.stream === 'sub' && profile.encoding?.toLowerCase() === 'h264'
			)?.stream ??
			camera.profiles.find((profile) => profile.encoding?.toLowerCase() === 'h264')?.stream ??
			camera.profiles.at(-1)?.stream ??
			'main'
		);
	}

	function previewSegment(
		camera: CameraListItem,
		segments: RecordingSegment[],
		targetMs: number | null
	): RecordingSegment | null {
		const preferred = preferredStream(camera);
		const preferredSegments = segments.filter((segment) => segment.stream === preferred);
		const candidates =
			preferredSegments.length > 0
				? preferredSegments
				: segments.filter((segment) => segment.stream !== preferred);
		if (targetMs === null) {
			return (
				candidates.toSorted((left, right) => left.start_time_ms - right.start_time_ms).at(-1) ??
				null
			);
		}
		return (
			candidates.find(
				(segment) => targetMs >= segment.start_time_ms && targetMs < segment.end_time_ms
			) ?? null
		);
	}

	async function loadFilmstrip(
		cameraList: CameraListItem[],
		selectedDate: string,
		signal: AbortSignal,
		version: number
	) {
		const responses = await controlClient.getRecordingsForDate(
			cameraList.map((camera) => camera.id),
			selectedDate,
			signal
		);
		const segmentsByCamera = new Map(
			responses.map((response) => [response.camera_id, response.segments] as const)
		);
		const results = cameraList.map<CameraRecordings>((camera) => ({
			camera,
			segments: segmentsByCamera.get(camera.id) ?? []
		}));
		if (!signal.aborted && version === requestVersion) recordings = results;
	}

	$effect(() => {
		const cameraList = cameras;
		const selectedDate = date;
		const version = ++requestVersion;
		if (!selectedDate || cameraList.length <= 1) {
			recordings = [];
			loading = false;
			return;
		}

		const controller = new AbortController();
		loading = true;
		void loadFilmstrip(cameraList, selectedDate, controller.signal, version)
			.catch(() => {})
			.finally(() => {
				if (!controller.signal.aborted && version === requestVersion) loading = false;
			});

		return () => controller.abort();
	});
</script>

{#if cameras.length > 1}
	<section class="space-y-2 border-t pt-3" aria-label="Other camera recordings">
		<header class="flex items-center gap-2">
			<ClapperboardIcon class="size-4 text-muted-foreground" />
			<h2 class="text-xs font-semibold">Other cameras</h2>
		</header>
		<div class="flex min-h-[6.2rem] gap-2 overflow-x-auto pb-1">
			{#if loading && recordings.length === 0}
				{#each placeholders as placeholder (placeholder)}
					<Skeleton class="aspect-video w-44 shrink-0 rounded-md sm:w-52" />
				{/each}
			{:else}
				{#each previews as preview (preview.camera.id)}
					{#key preview.segment?.url ?? `${preview.camera.id}:empty`}
						<RecordingFilmstripPreview
							camera={preview.camera}
							segment={preview.segment}
							{timestampMs}
							{playing}
							{playbackRate}
							{onselect}
						/>
					{/key}
				{/each}
			{/if}
		</div>
	</section>
{/if}
