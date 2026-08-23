<script lang="ts">
	import { replaceState } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onMount, tick } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import type { StoredMediaPlayback } from '$lib/control-client';
	import { parseKeepMode, type KeepMode } from '$lib/keep-modes';
	import { isKeyboardTypingTarget } from '$lib/keyboard-shortcuts';
	import type { CameraListItem, RecordingEvent, RecordingSegment } from '$lib/types';
	import KeepExportPanel from '$lib/components/KeepExportPanel.svelte';
	import ColdSeekState from '$lib/components/ColdSeekState.svelte';
	import KeepStories from '$lib/components/KeepStories.svelte';
	import KeepSwimlanes from '$lib/components/KeepSwimlanes.svelte';
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
	const timeFormatter = new Intl.DateTimeFormat(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});
	const controlClient = useControlClient();

	let cameras: CameraListItem[] = $state([]);
	let mode = $state<KeepMode>('timeline');
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
	let storedPlayback: StoredMediaPlayback | null = null;
	let playbackUrl: string | null = $state(null);
	let playbackAnchorMs = 0;
	let playbackVersion = 0;
	let playing = $state(false);
	let playbackRate = $state(1);
	let pendingSeekSeconds = 0;
	let pendingPlay = false;
	let loadVersion = 0;
	let shuttleDirection = $state<-1 | 0 | 1>(0);
	let shuttleSpeed = $state(1);
	let timelineFollowRequest = $state(0);
	let exportRangeStartMs: number | null = $state(null);
	let exportRangeEndMs: number | null = $state(null);
	let configuredFrameRates = $state.raw<ReadonlyMap<string, number>>(new Map());
	let coldSeekTimestampMs: number | null = $state(null);
	let coldSeekElapsedMs = $state(0);
	let coldSeekStartedAt = 0;

	let orderedSegments = $derived(
		segments
			.filter((segment) => segment.stream === stream)
			.toSorted((left, right) => left.start_time_ms - right.start_time_ms)
	);
	let availableStreams = $derived(new Set(segments.map((segment) => segment.stream)));
	let selectedCamera = $derived(cameras.find((camera) => camera.id === cameraId) ?? null);
	let selectedBitrateKbps = $derived(
		selectedCamera?.profiles.find((profile) => profile.stream === stream)?.bitrate_kbps ?? null
	);
	let dayStartMs = $derived(selectedDate ? Date.parse(`${selectedDate}T00:00:00Z`) : 0);
	let swimlaneAnchorMs = $derived.by(() => {
		if (playheadMs !== null) return playheadMs;
		const latestSegment = orderedSegments.at(-1);
		if (latestSegment !== undefined) return latestSegment.end_time_ms;
		return dayStartMs > 0 ? dayStartMs + 12 * 60 * 60_000 : Date.now();
	});
	let dateIndex = $derived(dates.indexOf(selectedDate));
	let olderDate = $derived(dateIndex >= 0 ? (dates[dateIndex + 1] ?? null) : null);
	let newerDate = $derived(dateIndex > 0 ? (dates[dateIndex - 1] ?? null) : null);
	let frameDurationSeconds = $derived.by(() => {
		const frameRate = configuredFrameRates.get(`${cameraId}:${stream}`);
		return frameRate && frameRate > 0 ? 1 / frameRate : null;
	});

	$effect(() => {
		if (coldSeekTimestampMs === null) return;
		const updateElapsed = () => {
			coldSeekElapsedMs = Math.max(0, performance.now() - coldSeekStartedAt);
		};
		updateElapsed();
		const timer = window.setInterval(updateElapsed, 100);
		return () => window.clearInterval(timer);
	});

	$effect(() => {
		if (shuttleDirection !== -1 || !selected) return;
		const reverse = () => {
			const current = playheadMs ?? selected?.start_time_ms;
			if (current !== undefined) seekToTimestamp(current - shuttleSpeed * 250, false);
		};
		reverse();
		const timer = window.setInterval(reverse, 250);
		return () => window.clearInterval(timer);
	});

	onMount(() => {
		void initialize();
		return () => {
			playbackVersion += 1;
			void closeStoredPlayback();
		};
	});

	async function initialize() {
		try {
			const params = new URLSearchParams(window.location.search);
			mode = parseKeepMode(params.get('mode'));
			const requestedTimestampMs = parseTimestamp(params.get('at'));
			const [nextCameras, health] = await Promise.all([
				controlClient.getCameras(),
				controlClient.getHealth().catch(() => null)
			]);
			cameras = nextCameras;
			configuredFrameRates = new Map(
				(health?.cameras ?? []).flatMap((camera) =>
					camera.configured_profiles.flatMap((profile) =>
						profile.framerate && profile.framerate > 0
							? [[`${camera.id}:${profile.stream}`, profile.framerate] as const]
							: []
					)
				)
			);
			const requestedCamera = params.get('camera');
			cameraId = cameras.some((camera) => camera.id === requestedCamera)
				? requestedCamera!
				: (cameras[0]?.id ?? '');
			const requestedStream = params.get('stream');
			if (requestedStream === 'main' || requestedStream === 'sub') stream = requestedStream;
			const requestedDate =
				params.get('date') ??
				(requestedTimestampMs === null
					? undefined
					: new Date(requestedTimestampMs).toISOString().slice(0, 10));
			if (cameraId) {
				await loadRecordings(requestedDate, requestedTimestampMs ?? undefined, false);
			}
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
			const response = await controlClient.getRecordings(cameraId, date);
			if (version !== loadVersion) return;
			segments = response.segments;
			dates = response.dates;
			selectedDate = response.date ?? date ?? '';
			if (selectedDate) {
				try {
					const eventResponse = await controlClient.getRecordingEvents(cameraId, selectedDate);
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

	function selectFilmstripCamera(nextCameraId: string, timestampMs: number) {
		if (nextCameraId === cameraId) return;
		const play = video !== null && !video.paused;
		cameraId = nextCameraId;
		void loadRecordings(selectedDate || undefined, timestampMs, play);
	}

	function openTimestamp(timestampMs: number): void {
		mode = 'timeline';
		seekToTimestamp(timestampMs, true);
		updateUrl();
	}

	function selectSwimlane(nextCameraId: string, timestampMs: number): void {
		mode = 'timeline';
		cameraId = nextCameraId;
		void loadRecordings(selectedDate || undefined, timestampMs, false);
	}

	function switchMode(nextMode: KeepMode): void {
		if (nextMode === mode) return;
		mode = nextMode;
		updateUrl();
	}

	function modeLabel(value: KeepMode): string {
		return value.charAt(0).toUpperCase() + value.slice(1);
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
			playbackVersion += 1;
			await closeStoredPlayback();
			selected = null;
			playheadMs = null;
			playing = false;
			return;
		}
		const sameSegment = selected?.url === segment.url && storedPlayback !== null;
		const requestedTimestampMs =
			segment.start_time_ms +
			Math.max(0, Math.min(offsetSeconds, Math.max(0, segment.duration_ms / 1_000 - 0.001))) *
				1_000;
		if (selected?.url !== segment.url) {
			exportRangeStartMs = null;
			exportRangeEndMs = null;
		}
		const requestedOffsetSeconds = (requestedTimestampMs - segment.start_time_ms) / 1_000;
		playerError = null;
		if (sameSegment) {
			selected = segment;
			pendingPlay = play;
			playing = play;
			playheadMs = requestedTimestampMs;
			pendingSeekSeconds = Math.max(0, (playheadMs - playbackAnchorMs) / 1_000);
			await tick();
			applyPendingSeek();
			return;
		}
		const version = ++playbackVersion;
		const previousPlayback = storedPlayback;
		if (previousPlayback) {
			video?.pause();
			previousPlayback.setPlaying(false);
			coldSeekTimestampMs = requestedTimestampMs;
			coldSeekElapsedMs = 0;
			coldSeekStartedAt = performance.now();
		}
		let playback: StoredMediaPlayback;
		try {
			playback = await controlClient.openStoredMedia({
				sourceId: cameraId,
				streamId: segment.stream,
				timestampMs: requestedTimestampMs,
				endTimeMs: segment.end_time_ms,
				playing: play,
				playbackRate
			});
		} catch (cause) {
			coldSeekTimestampMs = null;
			coldSeekElapsedMs = 0;
			if (previousPlayback) {
				playerError =
					cause instanceof Error ? cause.message : 'This recording could not be opened.';
				return;
			}
			throw cause;
		}
		if (version !== playbackVersion) {
			await playback.close().catch(() => undefined);
			return;
		}
		coldSeekTimestampMs = null;
		coldSeekElapsedMs = 0;
		selected = segment;
		pendingPlay = play;
		playing = play;
		playheadMs = requestedTimestampMs;
		storedPlayback = playback;
		playbackUrl = playback.url;
		playbackAnchorMs = playback.anchorTimeMs;
		pendingSeekSeconds = playback.initialOffsetSeconds;
		await tick();
		applyPendingSeek();
		if (previousPlayback && previousPlayback !== playback) {
			await previousPlayback.close().catch(() => undefined);
		}
	}

	async function closeStoredPlayback() {
		const playback = storedPlayback;
		storedPlayback = null;
		playbackUrl = null;
		if (playback) await playback.close().catch(() => undefined);
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

	function nextShuttleSpeed(direction: -1 | 1): number {
		if (shuttleDirection !== direction) return 1;
		const speeds = [1, 2, 4, 8];
		return speeds[(speeds.indexOf(shuttleSpeed) + 1) % speeds.length];
	}

	function setPlaybackSpeed(speed: number): void {
		shuttleSpeed = speed;
		playbackRate = speed;
		if (video) video.playbackRate = speed;
		storedPlayback?.setPlaybackRate(speed);
	}

	function setPlaying(nextPlaying: boolean): void {
		playing = nextPlaying;
		storedPlayback?.setPlaying(nextPlaying);
		if (!video) {
			pendingPlay = nextPlaying;
			return;
		}
		if (nextPlaying) {
			void video.play().catch(() => (playing = false));
		} else {
			video.pause();
		}
	}

	function shuttle(direction: -1 | 1): void {
		if (!selected) return;
		setPlaybackSpeed(nextShuttleSpeed(direction));
		shuttleDirection = direction;
		setPlaying(direction === 1);
	}

	function pauseTransport(): void {
		shuttleDirection = 0;
		setPlaying(false);
	}

	function toggleTransport(): void {
		if (playing || shuttleDirection === -1) {
			pauseTransport();
			return;
		}
		shuttleDirection = 1;
		setPlaying(true);
	}

	function stepFrame(direction: -1 | 1): void {
		if (!selected || frameDurationSeconds === null) return;
		pauseTransport();
		skip(direction * frameDurationSeconds);
	}

	function jumpToLive(): void {
		const latest = orderedSegments.at(-1);
		if (!latest) return;
		setPlaybackSpeed(1);
		shuttleDirection = 1;
		timelineFollowRequest += 1;
		seekToTimestamp(latest.end_time_ms - 1, true);
	}

	function setExportBoundary(boundary: 'start' | 'end'): void {
		if (playheadMs === null) return;
		if (boundary === 'start') exportRangeStartMs = playheadMs;
		else exportRangeEndMs = playheadMs;
	}

	function handleKeyboard(event: KeyboardEvent): void {
		if (isKeyboardTypingTarget(event.target) || event.metaKey || event.ctrlKey || event.altKey) {
			return;
		}
		const target = event.target instanceof Element ? event.target : null;
		const playerFocused = target?.closest('[data-keep-player]') !== null;
		const key = event.key.toLowerCase();
		if (key === 'j') shuttle(-1);
		else if (key === 'k') pauseTransport();
		else if (key === 'l') shuttle(1);
		else if (event.key === ' ' && !event.repeat) toggleTransport();
		else if (event.key === '[') setExportBoundary('start');
		else if (event.key === ']') setExportBoundary('end');
		else if (event.key === 'ArrowLeft' && playerFocused) stepFrame(-1);
		else if (event.key === 'ArrowRight' && playerFocused) stepFrame(1);
		else if (event.key === 'Home' && playerFocused) jumpToLive();
		else return;
		event.preventDefault();
	}

	function applyPendingSeek() {
		if (!video || !selected) return;
		video.playbackRate = playbackRate;
		const requestedTime = Math.max(
			0,
			Math.min(pendingSeekSeconds, video.duration || pendingSeekSeconds)
		);
		video.currentTime = requestedTime;
		if (Math.abs(video.currentTime - requestedTime) < 0.001) {
			playheadMs = playbackAnchorMs + video.currentTime * 1_000;
		}
		storedPlayback?.observe(video.currentTime);
		if (pendingPlay) void video.play().catch(() => (playing = false));
		pendingPlay = false;
	}

	function updatePlayhead() {
		if (!video || !selected) return;
		playheadMs = playbackAnchorMs + video.currentTime * 1_000;
		storedPlayback?.observe(video.currentTime);
	}

	function updatePlaybackRate() {
		if (!video) return;
		playbackRate = video.playbackRate;
		storedPlayback?.setPlaybackRate(playbackRate);
	}

	function handlePlay() {
		playing = true;
		shuttleDirection = 1;
		storedPlayback?.setPlaying(true);
	}

	function handlePause() {
		playing = false;
		if (shuttleDirection === 1) shuttleDirection = 0;
		storedPlayback?.setPlaying(false);
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

	function updateUrl() {
		if (!cameraId) return;
		const search = new URLSearchParams({
			camera: cameraId,
			stream,
			...(selectedDate ? { date: selectedDate } : {}),
			...(mode === 'timeline' ? {} : { mode })
		});
		// The base path is resolved before the query string is appended.
		// eslint-disable-next-line svelte/no-navigation-without-resolve
		replaceState(`${resolve('/keep')}?${search}`, {});
	}

	function formatDate(date: string): string {
		return dateFormatter.format(new Date(`${date}T12:00:00Z`));
	}

	function formatTime(timestampMs: number): string {
		return timeFormatter.format(new Date(timestampMs));
	}

	function parseTimestamp(value: string | null): number | null {
		if (value === null || value.trim() === '') return null;
		const timestampMs = Number(value);
		return Number.isSafeInteger(timestampMs) && timestampMs > 0 ? timestampMs : null;
	}
</script>

<svelte:window onkeydowncapture={handleKeyboard} />

<svelte:head>
	<title>Keep - KeepPeek</title>
</svelte:head>

<div class="mx-auto max-w-[120rem] space-y-4">
	<header class="flex flex-col gap-3 border-b pb-3 lg:flex-row lg:items-end lg:justify-between">
		<div class="flex min-h-9 flex-wrap items-center gap-3">
			<div class="flex items-center gap-2">
				<ArchiveIcon class="size-5 text-primary" />
				<h1 class="text-xl font-semibold">Keep</h1>
			</div>
			<div class="flex rounded-sm border border-hairline bg-raised p-0.5" aria-label="Keep modes">
				{#each ['timeline', 'stories', 'swimlanes', 'export'] as nextMode (nextMode)}
					<button
						type="button"
						class="h-7 rounded-xs px-2.5 text-2xs font-semibold {mode === nextMode
							? 'bg-primary text-on-primary'
							: 'text-text-muted hover:text-foreground'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						aria-pressed={mode === nextMode}
						onclick={() => switchMode(nextMode as KeepMode)}
					>
						{modeLabel(nextMode as KeepMode)}
					</button>
				{/each}
			</div>
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
		<div class="grid gap-4 lg:grid-cols-[minmax(0,1fr)_24.75rem]">
			<Skeleton class="aspect-video w-full rounded-md" />
			<Skeleton class="h-[34rem] w-full rounded-md" />
		</div>
	{:else if mode === 'stories'}
		<KeepStories {events} {dates} {selectedDate} ondate={changeDate} onseek={openTimestamp} />
	{:else if mode === 'swimlanes'}
		<KeepSwimlanes
			{cameras}
			selectedCameraId={cameraId}
			date={selectedDate}
			anchorMs={swimlaneAnchorMs}
			{playheadMs}
			onselect={selectSwimlane}
		/>
	{:else if mode === 'export'}
		{#key selected?.url ?? 'empty-export'}
			<KeepExportPanel
				sourceId={cameraId}
				sourceName={selectedCamera?.name ?? cameraId}
				segment={selected}
				bitrateKbps={selectedBitrateKbps}
				rangeStartMs={exportRangeStartMs}
				rangeEndMs={exportRangeEndMs}
			/>
		{/key}
	{:else}
		<div class="grid min-h-0 items-start gap-4 lg:grid-cols-[minmax(0,1fr)_24.75rem]">
			<section
				data-keep-player
				data-keyboard-shuttle-direction={shuttleDirection}
				data-keyboard-shuttle-speed={shuttleSpeed}
				data-keyboard-playing={playing}
				data-recording-playhead-ms={playheadMs}
				class="min-w-0 space-y-3"
				aria-label="Recorded video player"
			>
				<div class="relative overflow-hidden rounded-md bg-black ring-1 ring-black/10">
					{#if selected && playbackUrl}
						{#key selected.url}
							<!-- svelte-ignore a11y_media_has_caption (security camera recordings do not include caption tracks) -->
							<video
								bind:this={video}
								controls
								playsinline
								preload="metadata"
								src={playbackUrl}
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
					{:else if selected}
						<div class="flex aspect-video items-center justify-center text-sm text-zinc-400">
							Loading recording…
						</div>
					{:else}
						<div class="flex aspect-video items-center justify-center text-sm text-zinc-400">
							No recordings for this date and stream.
						</div>
					{/if}
					{#if coldSeekTimestampMs !== null && coldSeekElapsedMs >= 400}
						<ColdSeekState
							timestampLabel={formatTime(coldSeekTimestampMs)}
							elapsedMs={coldSeekElapsedMs}
							detail="The current frame stays until the requested recording arrives"
							overlay
							class="absolute inset-0 z-20"
						/>
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
				followRequest={timelineFollowRequest}
				onSeek={seekToTimestamp}
			/>
		</div>
	{/if}
</div>
