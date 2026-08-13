<script lang="ts">
	import { replaceState } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onMount, tick } from 'svelte';
	import { getCameras, getRecordingEvents, getRecordings, renewRecordingActivity } from '$lib/api';
	import type { CameraListItem, RecordingEvent, RecordingSegment } from '$lib/types';
	import RecordingFilmstrip from '$lib/components/RecordingFilmstrip.svelte';
	import VerticalTimeline from '$lib/components/VerticalTimeline.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import ArchiveIcon from '@lucide/svelte/icons/archive';
	import CalendarDaysIcon from '@lucide/svelte/icons/calendar-days';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import RotateCwIcon from '@lucide/svelte/icons/rotate-cw';

	const dateFormatter = new Intl.DateTimeFormat(undefined, {
		weekday: 'short',
		month: 'short',
		day: 'numeric',
		year: 'numeric',
		timeZone: 'UTC'
	});

	let cameras: CameraListItem[] = $state([]);
	let cameraId = $state('');
	let stream = $state<'main' | 'sub'>('main');
	let dates: string[] = $state([]);
	let selectedDate = $state('');
	let segments: RecordingSegment[] = $state([]);
	let events = $state.raw<RecordingEvent[]>([]);
	let selected: RecordingSegment | null = $state(null);
	let playheadMs: number | null = $state(null);
	let loading = $state(true);
	let error: string | null = $state(null);
	let playerError: string | null = $state(null);
	let video: HTMLVideoElement | null = $state(null);
	let playing = $state(false);
	let playbackRate = $state(1);
	let pendingSeekSeconds = 0;
	let pendingPlay = false;
	let loadVersion = 0;

	let orderedSegments = $derived(
		segments
			.filter((segment) => segment.stream === stream)
			.toSorted((left, right) => left.start_time_ms - right.start_time_ms)
	);
	let availableStreams = $derived(new Set(segments.map((segment) => segment.stream)));
	let selectedCamera = $derived(cameras.find((camera) => camera.id === cameraId) ?? null);
	let dayStartMs = $derived(selectedDate ? Date.parse(`${selectedDate}T00:00:00Z`) : 0);
	let dateIndex = $derived(dates.indexOf(selectedDate));
	let olderDate = $derived(dateIndex >= 0 ? (dates[dateIndex + 1] ?? null) : null);
	let newerDate = $derived(dateIndex > 0 ? (dates[dateIndex - 1] ?? null) : null);

	onMount(() => {
		void initialize();
		const timer = window.setInterval(renewActivity, 10_000);
		return () => window.clearInterval(timer);
	});

	async function initialize() {
		try {
			cameras = await getCameras();
			const params = new URLSearchParams(window.location.search);
			const requestedCamera = params.get('camera');
			cameraId = cameras.some((camera) => camera.id === requestedCamera)
				? requestedCamera!
				: (cameras[0]?.id ?? '');
			const requestedStream = params.get('stream');
			if (requestedStream === 'main' || requestedStream === 'sub') stream = requestedStream;
			if (cameraId) await loadRecordings(params.get('date') ?? undefined);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to open Keep';
		} finally {
			loading = false;
		}
	}

	async function loadRecordings(date?: string, targetTimestampMs?: number, play = false) {
		if (!cameraId) return;
		const version = ++loadVersion;
		loading = true;
		error = null;
		playerError = null;
		events = [];
		try {
			const response = await getRecordings(cameraId, date);
			if (version !== loadVersion) return;
			segments = response.segments;
			dates = response.dates;
			selectedDate = response.date ?? date ?? '';
			if (selectedDate) {
				try {
					const eventResponse = await getRecordingEvents(cameraId, selectedDate);
					if (version !== loadVersion) return;
					events = eventResponse.events;
				} catch {
					if (version !== loadVersion) return;
					events = [];
				}
			}
			if (
				response.segments.length > 0 &&
				!response.segments.some((segment) => segment.stream === stream)
			) {
				stream = response.segments.some((segment) => segment.stream === 'main') ? 'main' : 'sub';
			}
			const candidates = response.segments
				.filter((segment) => segment.stream === stream)
				.toSorted((left, right) => left.start_time_ms - right.start_time_ms);
			const target =
				targetTimestampMs === undefined ? null : recordingTarget(candidates, targetTimestampMs);
			if (target) {
				await selectSegment(target.segment, target.offsetSeconds, play);
			} else {
				await selectSegment(candidates.at(-1) ?? null, 0, play);
			}
			updateUrl();
		} catch (cause) {
			if (version !== loadVersion) return;
			error = cause instanceof Error ? cause.message : 'Failed to load recordings';
			segments = [];
			events = [];
			selected = null;
			playheadMs = null;
		} finally {
			if (version === loadVersion) loading = false;
		}
	}

	function changeCamera() {
		const targetTimestampMs = playheadMs ?? undefined;
		const play = video !== null && !video.paused;
		void loadRecordings(selectedDate || undefined, targetTimestampMs, play);
	}

	function selectFilmstripCamera(nextCameraId: string) {
		if (nextCameraId === cameraId) return;
		const targetTimestampMs = playheadMs ?? undefined;
		const play = video !== null && !video.paused;
		cameraId = nextCameraId;
		void loadRecordings(selectedDate || undefined, targetTimestampMs, play);
	}

	function changeDate(date: string) {
		if (!date || date === selectedDate) return;
		void loadRecordings(date);
	}

	function changeStream(next: 'main' | 'sub') {
		if (next === stream || !availableStreams.has(next)) return;
		const targetTimestampMs = playheadMs;
		const play = video !== null && !video.paused;
		stream = next;
		const candidates = segments
			.filter((segment) => segment.stream === stream)
			.toSorted((left, right) => left.start_time_ms - right.start_time_ms);
		const target =
			targetTimestampMs === null ? null : recordingTarget(candidates, targetTimestampMs);
		void selectSegment(
			target?.segment ?? candidates.at(-1) ?? null,
			target?.offsetSeconds ?? 0,
			play
		);
		updateUrl();
	}

	function handlePlayerError(event: Event) {
		void event;
		playerError = 'This recording could not be played.';
	}

	async function selectSegment(segment: RecordingSegment | null, offsetSeconds = 0, play = false) {
		if (!segment) {
			selected = null;
			playheadMs = null;
			playing = false;
			return;
		}
		const sameSegment = selected?.url === segment.url;
		selected = segment;
		pendingSeekSeconds = Math.max(0, Math.min(offsetSeconds, segment.duration_ms / 1_000));
		pendingPlay = play;
		playing = play;
		playheadMs = segment.start_time_ms + pendingSeekSeconds * 1_000;
		playerError = null;
		renewActivity();
		await tick();
		if (sameSegment) applyPendingSeek();
	}

	function seekToTimestamp(timestampMs: number, play = true) {
		const target = recordingTarget(orderedSegments, timestampMs);
		if (target) void selectSegment(target.segment, target.offsetSeconds, play);
	}

	function recordingTarget(candidates: RecordingSegment[], timestampMs: number) {
		if (candidates.length === 0) return null;
		const containing = candidates.find(
			(segment) => timestampMs >= segment.start_time_ms && timestampMs < segment.end_time_ms
		);
		const segment =
			containing ??
			candidates.find((candidate) => candidate.start_time_ms >= timestampMs) ??
			candidates.at(-1)!;
		return {
			segment,
			offsetSeconds:
				Math.max(0, Math.min(segment.duration_ms, timestampMs - segment.start_time_ms)) / 1_000
		};
	}

	function skip(seconds: number) {
		if (!selected) return;
		seekToTimestamp(
			(playheadMs ?? selected.start_time_ms) + seconds * 1_000,
			video !== null && !video.paused
		);
	}

	function applyPendingSeek() {
		if (!video || !selected) return;
		video.playbackRate = playbackRate;
		video.currentTime = Math.max(
			0,
			Math.min(pendingSeekSeconds, video.duration || pendingSeekSeconds)
		);
		playheadMs = selected.start_time_ms + video.currentTime * 1_000;
		if (pendingPlay) void video.play().catch(() => (playing = false));
		pendingPlay = false;
	}

	function updatePlayhead() {
		if (!video || !selected) return;
		playheadMs = selected.start_time_ms + video.currentTime * 1_000;
	}

	function updatePlaybackRate() {
		if (video) playbackRate = video.playbackRate;
	}

	function handlePlay() {
		playing = true;
		renewActivity();
	}

	function handlePause() {
		playing = false;
	}

	function handleEnded() {
		playing = false;
		playNext();
	}

	function playNext() {
		if (!selected) return;
		const index = orderedSegments.findIndex((segment) => segment.url === selected?.url);
		const next = orderedSegments[index + 1];
		if (next) void selectSegment(next, 0, true);
	}

	function renewActivity() {
		if (!cameraId || !selected) return;
		void renewRecordingActivity(cameraId, selected.stream).catch(() => {});
	}

	function updateUrl() {
		if (!cameraId) return;
		const search = new URLSearchParams({
			camera: cameraId,
			stream,
			...(selectedDate ? { date: selectedDate } : {})
		});
		// The base path is resolved before the query string is appended.
		// eslint-disable-next-line svelte/no-navigation-without-resolve
		replaceState(`${resolve('/keep')}?${search}`, {});
	}

	function formatDate(date: string): string {
		return dateFormatter.format(new Date(`${date}T12:00:00Z`));
	}
</script>

<svelte:head>
	<title>Keep - KeepPeek</title>
</svelte:head>

<div class="mx-auto max-w-[120rem] space-y-4">
	<header class="flex flex-col gap-3 border-b pb-3 lg:flex-row lg:items-end lg:justify-between">
		<div class="flex min-h-9 items-center gap-2">
			<ArchiveIcon class="size-5 text-primary" />
			<h1 class="text-xl font-semibold">Keep</h1>
		</div>

		<div class="flex flex-wrap items-end gap-2">
			<label class="grid gap-1 text-xs font-medium text-muted-foreground">
				Camera
				<select
					bind:value={cameraId}
					onchange={changeCamera}
					class="h-9 min-w-44 rounded-md border bg-background px-3 text-sm text-foreground"
				>
					{#each cameras as camera (camera.id)}
						<option value={camera.id}>{camera.name ?? camera.ip}</option>
					{/each}
				</select>
			</label>

			<div class="grid gap-1">
				<span class="text-xs font-medium text-muted-foreground">Date</span>
				<div class="flex items-center rounded-md border bg-background">
					<Button
						variant="ghost"
						size="icon"
						title="Previous recorded day"
						disabled={!olderDate}
						onclick={() => olderDate && changeDate(olderDate)}
					>
						<ChevronLeftIcon />
					</Button>
					<label class="relative flex h-9 items-center gap-2 border-x px-2">
						<CalendarDaysIcon class="size-4 text-muted-foreground" />
						<select
							value={selectedDate}
							disabled={dates.length === 0}
							class="appearance-none bg-transparent pr-4 text-sm outline-none"
							onchange={(event) => changeDate(event.currentTarget.value)}
						>
							{#each dates as date (date)}
								<option value={date}>{formatDate(date)}</option>
							{/each}
						</select>
					</label>
					<Button
						variant="ghost"
						size="icon"
						title="Next recorded day"
						disabled={!newerDate}
						onclick={() => newerDate && changeDate(newerDate)}
					>
						<ChevronRightIcon />
					</Button>
				</div>
			</div>

			{#if availableStreams.size > 1}
				<div class="grid gap-1">
					<span class="text-xs font-medium text-muted-foreground">Stream</span>
					<div class="flex rounded-md border bg-background p-0.5">
						<Button
							variant={stream === 'main' ? 'secondary' : 'ghost'}
							size="sm"
							onclick={() => changeStream('main')}>Main</Button
						>
						<Button
							variant={stream === 'sub' ? 'secondary' : 'ghost'}
							size="sm"
							onclick={() => changeStream('sub')}>Sub</Button
						>
					</div>
				</div>
			{/if}

			<Button
				variant="outline"
				size="icon"
				title="Refresh recordings"
				disabled={!cameraId || loading}
				onclick={() => void loadRecordings(selectedDate || undefined)}
			>
				<RefreshCwIcon class={loading ? 'animate-spin' : ''} />
			</Button>
		</div>
	</header>

	{#if error}
		<div
			class="rounded-md border border-destructive/60 bg-destructive/10 px-4 py-3 text-sm text-destructive"
		>
			{error}
		</div>
	{/if}

	{#if loading && segments.length === 0}
		<div class="grid gap-4 lg:grid-cols-[minmax(0,1fr)_15rem] xl:grid-cols-[minmax(0,1fr)_18rem]">
			<Skeleton class="aspect-video w-full rounded-md" />
			<Skeleton class="h-[34rem] w-full rounded-md" />
		</div>
	{:else}
		<div
			class="grid min-h-0 items-start gap-4 lg:grid-cols-[minmax(0,1fr)_15rem] xl:grid-cols-[minmax(0,1fr)_18rem]"
		>
			<section class="min-w-0 space-y-3" aria-label="Recorded video player">
				<div class="relative overflow-hidden rounded-md bg-black ring-1 ring-black/10">
					{#if selected}
						{#key selected.url}
							<!-- svelte-ignore a11y_media_has_caption (security camera recordings do not include caption tracks) -->
							<video
								bind:this={video}
								controls
								playsinline
								preload="metadata"
								src={selected.url}
								class="aspect-video w-full object-contain"
								onloadedmetadata={applyPendingSeek}
								ondurationchange={applyPendingSeek}
								ontimeupdate={updatePlayhead}
								onended={handleEnded}
								onplay={handlePlay}
								onpause={handlePause}
								onratechange={updatePlaybackRate}
								onerror={handlePlayerError}
							></video>
						{/key}
						<span
							class="pointer-events-none absolute top-3 left-3 max-w-[calc(100%-1.5rem)] truncate rounded-sm bg-black/72 px-2 py-1 text-xs font-semibold text-white shadow-sm backdrop-blur-sm"
						>
							{selectedCamera?.name ?? selectedCamera?.id ?? cameraId}
						</span>
					{:else}
						<div class="flex aspect-video items-center justify-center text-sm text-zinc-400">
							No recordings for this date and stream.
						</div>
					{/if}
				</div>

				{#if playerError}
					<p class="text-sm text-destructive">{playerError}</p>
				{/if}

				<div
					class="flex min-h-10 items-center justify-end gap-1 border-b pb-3"
					aria-label="Playback controls"
				>
					<Button
						variant="outline"
						size="icon-sm"
						title="Back 10 seconds"
						disabled={!selected}
						onclick={() => skip(-10)}
					>
						<RotateCcwIcon />
					</Button>
					<Button
						variant="outline"
						size="icon-sm"
						title="Forward 10 seconds"
						disabled={!selected}
						onclick={() => skip(10)}
					>
						<RotateCwIcon />
					</Button>
				</div>

				<RecordingFilmstrip
					{cameras}
					selectedCameraId={cameraId}
					date={selectedDate}
					timestampMs={playheadMs}
					{playing}
					{playbackRate}
					onselect={selectFilmstripCamera}
				/>
			</section>

			<VerticalTimeline
				segments={orderedSegments}
				{events}
				selectedUrl={selected?.url ?? null}
				{playheadMs}
				{dayStartMs}
				onSeek={seekToTimestamp}
			/>
		</div>
	{/if}
</div>
