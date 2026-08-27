<script lang="ts">
	import { replaceState } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onMount, tick } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import type {
		EventPreviewHit,
		StoredMediaKeyFramePreview,
		StoredMediaPlayback,
		StoredMediaStartupPhase
	} from '$lib/control-client';
	import { decodeEventKeyframePreview } from '$lib/event-keyframe-preview';
	import { emitTimelinePerformanceEvent } from '$lib/timeline-observability';
	import {
		TimelineRepository,
		type TimelineInterval,
		type TimelineViewport
	} from '$lib/timeline-repository.svelte';
	import { parseKeepMode, type KeepMode } from '$lib/keep-modes';
	import { isKeyboardTypingTarget } from '$lib/keyboard-shortcuts';
	import {
		browserSupportsRecordedEncoding,
		selectRecordedStream,
		type RecordedQualityPreference,
		type RecordedStreamId,
		type RecordedStreamSelection
	} from '$lib/recorded-playback-policy';
	import {
		defaultPlaybackPreferences,
		loadPlaybackPreferences,
		recordedPreference,
		savePlaybackPreferences,
		withMediaPreferences,
		withRecordedPreference
	} from '$lib/playback-preferences';
	import type { CameraListItem, RecordingEvent, RecordingSegment } from '$lib/types';
	import KeepCameraSwitcher from '$lib/components/KeepCameraSwitcher.svelte';
	import KeepExportPanel from '$lib/components/KeepExportPanel.svelte';
	import ColdSeekState from '$lib/components/ColdSeekState.svelte';
	import KeepStories from '$lib/components/KeepStories.svelte';
	import KeepSwimlanes from '$lib/components/KeepSwimlanes.svelte';
	import HorizontalTimeline from '$lib/components/HorizontalTimeline.svelte';
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
		second: '2-digit',
		timeZone: 'UTC'
	});
	const initialRecordingWindowMs = 5 * 60_000;
	const maximumRecordingWindowMs = 6 * 60 * 60_000;
	const cameraSwitchDurationMs = 180;
	const startupPhaseTimeoutMs = 3_000;
	const recordedQualityOptions: ReadonlyArray<{
		value: RecordedQualityPreference;
		label: string;
	}> = [
		{ value: 'auto', label: 'Auto' },
		{ value: 'high', label: 'High' },
		{ value: 'low', label: 'Low' },
		{ value: 'main', label: 'Main exact' },
		{ value: 'sub', label: 'Sub exact' }
	];
	const controlClient = useControlClient();
	const timelineRepository = new TimelineRepository(controlClient);

	let cameras: CameraListItem[] = $state([]);
	let mode = $state<KeepMode>('timeline');
	let cameraId = $state('');
	let stream = $state<'main' | 'sub'>('main');
	let dates: string[] = $state([]);
	let recordingDatesPending = false;
	let selectedDate = $state('');
	let segments: RecordingSegment[] = $state([]);
	let storyModeEvents = $state.raw<RecordingEvent[]>([]);
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
	let playbackPreferences = $state.raw(defaultPlaybackPreferences());
	let playbackMuted = $state(false);
	let playbackNotice = $state<string | null>(null);
	let requestedPlaybackVariant = $state('auto');
	let selectedPlaybackVariant = $state<RecordedStreamId | null>(null);
	let selectedPlaybackReason = $state('automatic');
	let selectedFallbackStream = $state<RecordedStreamId | null>(null);
	let playbackContentType = $state<string | null>(null);
	let playbackStartupPhase = $state<'idle' | 'opening' | StoredMediaStartupPhase | 'first-frame'>(
		'idle'
	);
	let keepNavigationStartedAtMs = 0;
	let keepFirstSegmentEmitted = false;
	let fallbackStreams = $state.raw<RecordedStreamId[]>([]);
	let rejectedStreams = $state.raw<Array<{ stream: RecordedStreamId; encoding: string }>>([]);
	let fallbackAttempted = false;
	let playbackFailureHandling = false;
	let playbackOpenController: AbortController | null = null;
	let playbackStartupTimer: ReturnType<typeof setTimeout> | null = null;
	let playbackErrorUnsubscribe: (() => void) | null = null;
	let playbackStartupUnsubscribe: (() => void) | null = null;
	let cameraProfilesPromise: Promise<CameraListItem[]> | null = null;
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
	let exportSeedEvent = $state.raw<RecordingEvent | null>(null);
	let configuredFrameRates = $state.raw<ReadonlyMap<string, number>>(new Map());
	let coldSeekTimestampMs: number | null = $state(null);
	let coldSeekElapsedMs = $state(0);
	let coldSeekStartedAt = 0;
	let recordingLoadController: AbortController | null = null;
	let targetLoadController: AbortController | null = null;
	let targetLoadVersion = 0;
	let recordingCoverage = $state.raw<TimelineInterval[]>([]);
	let latestTimelineViewport = $state.raw<TimelineViewport | null>(null);
	let eventSearchAvailable = $state(false);
	let stillPreviewUrl = $state<string | null>(null);
	let previewVersion = 0;
	let previewController: AbortController | null = null;
	const keyframePreviewCache = new Map<string, string>();
	let keyFrameUnsubscribe: (() => void) | null = null;
	let scrubbing = $state(false);
	let scrubTargetMs: number | null = null;
	let scrubPump: Promise<void> | null = null;
	let scrubVersion = 0;
	let scrubPlayIntent = false;
	let scrubOpenController: AbortController | null = null;
	let ignoreNextPauseEvent = false;
	let mobilePortrait = $state(false);
	let capabilitiesSeen = false;
	let reconnectPending = false;
	let scrubUsesFragmentFallback = false;
	let secondaryLoadsReady = $state(false);
	let secondaryLoadsTimer: number | null = null;
	let cameraSwitchDirection = $state<-1 | 1>(1);
	let cameraSwitchPending = $state(false);
	let cameraSwitchAnimating = $state(false);
	let cameraSwitchFrameUrl = $state<string | null>(null);
	let cameraSwitchVersion = 0;
	let cameraSwitchTimer: number | null = null;

	let viewportSegments = $derived(
		timelineRepository.ranges.flatMap((range): RecordingSegment[] => {
			if (
				range.sourceId !== cameraId ||
				(range.streamId !== 'main' && range.streamId !== 'sub') ||
				!selectedDate
			) {
				return [];
			}
			return [recordingSegment(range.streamId, selectedDate, range.startMs, range.endMs)];
		})
	);
	let allSegments = $derived(mergeRecordingSegments([...segments, ...viewportSegments]));
	let playableSegments = $derived(
		segments
			.filter((segment) => segment.stream === stream)
			.toSorted((left, right) => left.start_time_ms - right.start_time_ms)
	);
	let orderedSegments = $derived(
		allSegments
			.filter((segment) => segment.stream === stream)
			.toSorted((left, right) => left.start_time_ms - right.start_time_ms)
	);
	let availableStreams = $derived(new Set(allSegments.map((segment) => segment.stream)));
	let selectedCamera = $derived(cameras.find((camera) => camera.id === cameraId) ?? null);
	let selectedBitrateKbps = $derived(
		selectedCamera?.profiles.find((profile) => profile.stream === stream)?.bitrate_kbps ?? null
	);
	let dayStartMs = $derived(selectedDate ? Date.parse(`${selectedDate}T00:00:00Z`) : 0);
	let events = $derived(
		timelineRepository.events.filter(
			(event) => event.start_time_ms >= dayStartMs && event.start_time_ms < dayStartMs + 86_400_000
		)
	);
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
	let isLiveDate = $derived(
		selectedDate.length > 0 && selectedDate === new Date(Date.now()).toISOString().slice(0, 10)
	);

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
		if (!secondaryLoadsReady || !isLiveDate || mode !== 'timeline') return;
		const timer = window.setInterval(() => {
			const viewport = latestTimelineViewport;
			if (!viewport) return;
			const refreshStartMs = Math.max(viewport.startMs, viewport.endMs - 5 * 60_000);
			timelineRepository.invalidate(refreshStartMs, viewport.endMs + viewport.bucketMs);
			void loadTimelineViewport(viewport);
		}, 5_000);
		return () => window.clearInterval(timer);
	});

	$effect(() => {
		if (mode === 'timeline' || !recordingDatesPending) return;
		recordingDatesPending = false;
		void discoverRecordingDates(false);
	});

	$effect(() => {
		if (!secondaryLoadsReady || mode !== 'stories' || !cameraId || !selectedDate) {
			storyModeEvents = [];
			return;
		}
		const controller = new AbortController();
		const dayStart = Date.parse(`${selectedDate}T00:00:00Z`);
		const dayEnd = dayStart + 86_400_000;
		const endMs = Math.min(dayEnd, Math.max(dayStart + 1, swimlaneAnchorMs));
		const startMs = Math.max(dayStart, endMs - 6 * 60 * 60_000);
		void controlClient
			.searchEventMetadata({
				sourceIds: [cameraId],
				streamId: 'main',
				startMs,
				endMs,
				eventTypes: ['story'],
				pageSize: 18,
				signal: controller.signal
			})
			.then((result) => {
				if (!controller.signal.aborted) storyModeEvents = result.hits.map(recordingEventFromHit);
			})
			.catch(() => {
				if (!controller.signal.aborted) storyModeEvents = [];
			});
		return () => controller.abort();
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
		keepNavigationStartedAtMs = performance.now();
		const portraitMedia = window.matchMedia('(max-width: 767px) and (orientation: portrait)');
		const updateOrientation = () => (mobilePortrait = portraitMedia.matches);
		updateOrientation();
		portraitMedia.addEventListener('change', updateOrientation);
		const unsubscribeCapabilities = controlClient.onCapabilities((capabilityIds) => {
			eventSearchAvailable = capabilityIds.includes('keeppeek.event-search');
			if (capabilityIds.length === 0) {
				if (capabilitiesSeen) reconnectPending = true;
				return;
			}
			if (reconnectPending) {
				reconnectPending = false;
				timelineRepository.revalidate();
				if (latestTimelineViewport) void loadTimelineViewport(latestTimelineViewport);
			}
			capabilitiesSeen = true;
		});
		void initialize();
		return () => {
			portraitMedia.removeEventListener('change', updateOrientation);
			unsubscribeCapabilities();
			if (secondaryLoadsTimer !== null) window.clearTimeout(secondaryLoadsTimer);
			if (cameraSwitchTimer !== null) window.clearTimeout(cameraSwitchTimer);
			if (cameraSwitchFrameUrl) URL.revokeObjectURL(cameraSwitchFrameUrl);
			previewController?.abort();
			for (const url of keyframePreviewCache.values()) URL.revokeObjectURL(url);
			recordingLoadController?.abort();
			targetLoadController?.abort();
			playbackOpenController?.abort();
			scrubOpenController?.abort();
			clearPlaybackStartupTimer();
			detachPlaybackObservers();
			timelineRepository.dispose();
			playbackVersion += 1;
			void closeStoredPlayback();
		};
	});

	async function initialize() {
		try {
			playbackPreferences = loadPlaybackPreferences(window.localStorage);
			playbackMuted = playbackPreferences.media.muted;
			playbackRate = playbackPreferences.media.playbackRate;
			const initialPlay = playbackPreferences.media.playing;
			const params = new URLSearchParams(window.location.search);
			mode = parseKeepMode(params.get('mode'));
			const requestedTimestampMs = parseTimestamp(params.get('at'));
			const requestedEventId = params.get('event')?.trim() ?? '';
			const requestedCamera = params.get('camera')?.trim() ?? '';
			const requestedStream = params.get('stream');
			const hasRequestedStream = requestedStream === 'main' || requestedStream === 'sub';
			if (hasRequestedStream) stream = requestedStream;
			const requestedDate =
				params.get('date') ??
				(requestedTimestampMs === null
					? undefined
					: new Date(requestedTimestampMs).toISOString().slice(0, 10));
			const resolveLatestDateFirst = requestedDate === undefined && requestedTimestampMs === null;
			const initialDate = requestedDate ?? new Date().toISOString().slice(0, 10);
			const camerasPromise = controlClient.getCameras().then((nextCameras) => {
				cameras = nextCameras;
				return nextCameras;
			});
			cameraProfilesPromise = camerasPromise;
			const healthPromise = controlClient.getHealth().catch(() => null);
			let recordingsPromise: Promise<void> | null = null;
			if (requestedCamera && hasRequestedStream && !resolveLatestDateFirst) {
				cameraId = requestedCamera;
				recordingsPromise = loadRecordings(
					initialDate,
					requestedTimestampMs ?? undefined,
					initialPlay,
					requestedStream
				);
			}

			const nextCameras = await camerasPromise;
			const resolvedCameraId = nextCameras.some((camera) => camera.id === requestedCamera)
				? requestedCamera
				: (nextCameras[0]?.id ?? '');
			if (resolveLatestDateFirst && resolvedCameraId) {
				cameraId = resolvedCameraId;
				try {
					dates = await controlClient.getRecordingDates(resolvedCameraId);
				} catch {
					dates = [];
				}
				recordingsPromise = loadRecordings(
					dates[0] ?? initialDate,
					undefined,
					initialPlay,
					hasRequestedStream ? requestedStream : null
				);
			} else if (cameraId !== resolvedCameraId) {
				cameraId = resolvedCameraId;
				recordingsPromise = cameraId
					? loadRecordings(
							initialDate,
							requestedTimestampMs ?? undefined,
							initialPlay,
							hasRequestedStream ? requestedStream : null
						)
					: null;
			} else if (!recordingsPromise && cameraId) {
				recordingsPromise = loadRecordings(
					initialDate,
					requestedTimestampMs ?? undefined,
					initialPlay,
					hasRequestedStream ? requestedStream : null
				);
			}

			const health = await healthPromise;
			configuredFrameRates = new Map(
				(health?.cameras ?? []).flatMap((camera) =>
					camera.configured_profiles.flatMap((profile) =>
						profile.framerate && profile.framerate > 0
							? [[`${camera.id}:${profile.stream}`, profile.framerate] as const]
							: []
					)
				)
			);
			if (recordingsPromise) {
				await recordingsPromise;
				if (!resolveLatestDateFirst) scheduleRecordingDateDiscovery();
			}
			if (requestedEventId && requestedTimestampMs !== null && cameraId) {
				exportSeedEvent = await resolveExportSeedEvent(
					requestedEventId,
					cameraId,
					requestedTimestampMs
				);
				if (!exportSeedEvent && mode === 'export') {
					error = 'The selected event revision is no longer available for export.';
				}
			}
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to open Keep';
		} finally {
			loading = false;
		}
	}

	async function loadRecordings(
		date?: string,
		targetTimestampMs?: number,
		play = true,
		requestedStream: RecordedStreamId | null = null
	) {
		if (!cameraId) return;
		deferSecondaryLoads();
		const version = ++loadVersion;
		playbackVersion += 1;
		scrubVersion += 1;
		scrubTargetMs = null;
		scrubOpenController?.abort();
		scrubOpenController = null;
		scrubbing = false;
		playbackOpenController?.abort();
		playbackOpenController = null;
		clearPlaybackStartupTimer();
		targetLoadVersion += 1;
		targetLoadController?.abort();
		recordingLoadController?.abort();
		const controller = new AbortController();
		recordingLoadController = controller;
		loading = true;
		error = null;
		playerError = null;
		const requestedDate = (date ?? selectedDate) || new Date().toISOString().slice(0, 10);
		selectedDate = requestedDate;
		segments = [];
		recordingCoverage = [];
		try {
			const response = await loadInitialRecordingWindow(
				requestedDate,
				targetTimestampMs,
				controller,
				version
			);
			if (version !== loadVersion) return;
			await cameraProfilesPromise?.catch(() => []);
			if (version !== loadVersion || controller.signal.aborted) return;
			segments = response.segments;
			recordingCoverage = response.coverage;
			if (response.segments.length > 0 && !dates.includes(requestedDate)) {
				dates = [...dates, requestedDate].toSorted().toReversed();
			}
			const selection = chooseRecordedStream(response.segments, requestedStream);
			if (selection.selectedStream === null) {
				await selectSegment(null);
				playerError =
					response.segments.length === 0 && targetTimestampMs !== undefined
						? 'No indexed footage is available near that time.'
						: unsupportedRecordedPlaybackMessage(selection);
				updateUrl();
				return;
			}
			stream = selection.selectedStream;
			const candidates = response.segments
				.filter((segment) => segment.stream === stream)
				.toSorted((left, right) => left.start_time_ms - right.start_time_ms);
			const target =
				targetTimestampMs === undefined ? null : recordingTarget(candidates, targetTimestampMs);
			if (target) {
				await selectSegment(target.segment, target.offsetSeconds, play);
			} else if (targetTimestampMs !== undefined) {
				await selectSegment(null);
				playerError = 'No indexed footage is available near that time.';
			} else {
				await selectSegment(candidates.at(-1) ?? null, 0, play);
			}
			emitKeepFirstSegment();
			updateUrl();
		} catch (cause) {
			if (version !== loadVersion) return;
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			cameraSwitchPending = false;
			cameraSwitchAnimating = false;
			clearCameraSwitchFrame();
			error = cause instanceof Error ? cause.message : 'Failed to load recordings';
			segments = [];
			selected = null;
			playheadMs = null;
		} finally {
			if (version === loadVersion) loading = false;
		}
	}

	function emitKeepFirstSegment(): void {
		if (keepFirstSegmentEmitted || !selected) return;
		keepFirstSegmentEmitted = true;
		emitTimelinePerformanceEvent('KeepFirstSegment', {
			sourceId: cameraId,
			streamId: selected.stream,
			durationMs: Math.max(0, performance.now() - keepNavigationStartedAtMs)
		});
	}

	function chooseRecordedStream(
		availableSegments: readonly RecordingSegment[],
		requestedStream: RecordedStreamId | null,
		preference = recordedPreference(playbackPreferences, cameraId)
	): RecordedStreamSelection {
		const selection = selectRecordedStream(selectedCamera, {
			availableStreams: new Set(availableSegments.map((segment) => segment.stream)),
			requestedStream,
			preference,
			isEncodingSupported: browserSupportsRecordedEncoding
		});
		requestedPlaybackVariant = requestedStream ?? preference;
		selectedPlaybackReason = selection.reason;
		selectedPlaybackVariant = selection.selectedStream;
		fallbackStreams = selection.fallbackStreams;
		rejectedStreams = selection.rejectedStreams;
		fallbackAttempted = false;
		playbackFailureHandling = false;
		selectedFallbackStream = null;
		playbackNotice = compatibilityFallbackNotice(selection, requestedStream);
		return selection;
	}

	function compatibilityFallbackNotice(
		selection: RecordedStreamSelection,
		requestedStream: RecordedStreamId | null
	): string | null {
		const rejected =
			selection.rejectedStreams.find((candidate) => candidate.stream === requestedStream) ??
			selection.rejectedStreams[0];
		if (!rejected || selection.selectedStream === null) return null;
		return `${streamLabel(rejected.stream)} uses ${rejected.encoding}, which this browser cannot decode. Playing ${streamLabel(selection.selectedStream)} instead.`;
	}

	function unsupportedRecordedPlaybackMessage(selection: RecordedStreamSelection): string {
		if (selection.rejectedStreams.length === 0) {
			return 'No recorded variant is available for this camera and time.';
		}
		const codecs = selection.rejectedStreams
			.map((candidate) => `${streamLabel(candidate.stream)} (${candidate.encoding})`)
			.join(', ');
		return `This browser cannot decode the available recordings: ${codecs}. Configure an H.264 recording profile; stored playback transcoding is not available.`;
	}

	function streamLabel(value: RecordedStreamId): string {
		return value === 'main' ? 'Main' : 'Sub';
	}

	async function loadInitialRecordingWindow(
		date: string,
		targetTimestampMs: number | undefined,
		controller: AbortController,
		version: number
	): Promise<{ segments: RecordingSegment[]; coverage: TimelineInterval[] }> {
		const dayStart = Date.parse(`${date}T00:00:00Z`);
		const dayEnd = dayStart + 86_400_000;
		const currentDay = new Date().toISOString().slice(0, 10);
		const edgeMs =
			targetTimestampMs ?? (date === currentDay ? Math.min(Date.now(), dayEnd) : dayEnd);
		let cursorMs = edgeMs;
		let durationMs = initialRecordingWindowMs;
		let accumulated: RecordingSegment[] = [];
		let coverage: TimelineInterval[] = [];
		let searchComplete = false;
		do {
			const startMs = Math.max(dayStart, cursorMs - durationMs);
			const endMs = Math.min(
				dayEnd,
				targetTimestampMs === undefined ? cursorMs : edgeMs + durationMs
			);
			const [response] = await controlClient.getRecordingsInRange(
				[cameraId],
				date,
				startMs,
				endMs,
				controller.signal,
				(pages) => {
					if (version !== loadVersion || !pages[0]) return;
					segments = mergeRecordingSegments([...accumulated, ...pages[0].segments]);
				}
			);
			if (version !== loadVersion || controller.signal.aborted) {
				throw new DOMException('Recording query was cancelled.', 'AbortError');
			}
			accumulated = mergeRecordingSegments([...accumulated, ...response.segments]);
			coverage = [...coverage, { startMs, endMs }];
			searchComplete =
				targetTimestampMs !== undefined ||
				accumulated.length > 0 ||
				(startMs === dayStart && endMs === dayEnd) ||
				startMs === dayStart;
			if (!searchComplete) {
				if (targetTimestampMs === undefined) cursorMs = startMs;
				durationMs = Math.min(durationMs * 2, maximumRecordingWindowMs);
			}
		} while (!searchComplete);
		return { segments: accumulated, coverage };
	}

	async function discoverRecordingDates(selectLatestWhenEmpty: boolean): Promise<void> {
		const sourceId = cameraId;
		const version = loadVersion;
		try {
			const nextDates = await controlClient.getRecordingDates(sourceId);
			if (version !== loadVersion || sourceId !== cameraId) return;
			dates = nextDates;
			if (
				selectLatestWhenEmpty &&
				segments.length === 0 &&
				nextDates[0] &&
				nextDates[0] !== selectedDate
			) {
				await loadRecordings(nextDates[0], undefined, playbackIntent());
			}
		} catch {
			if (version === loadVersion && segments.length > 0 && !dates.includes(selectedDate)) {
				dates = [selectedDate];
			}
		}
	}

	function handleTimelineViewport(viewport: TimelineViewport): void {
		latestTimelineViewport = viewport;
		if (secondaryLoadsReady) void loadTimelineViewport(viewport);
	}

	async function loadTimelineViewport(viewport: TimelineViewport): Promise<void> {
		if (!cameraId || !selectedDate || mode !== 'timeline') return;
		await timelineRepository
			.loadWindow({
				...viewport,
				sourceIds: [cameraId],
				minimumMs: dayStartMs,
				maximumMs: isLiveDate ? Date.now() : dayStartMs + 86_400_000
			})
			.catch(() => undefined);
		if (recordingDatesPending) {
			recordingDatesPending = false;
			void discoverRecordingDates(false);
		}
	}

	function scheduleRecordingDateDiscovery(): void {
		recordingDatesPending = true;
		if (mode !== 'timeline') {
			recordingDatesPending = false;
			void discoverRecordingDates(false);
		}
	}

	function showNearestCachedPreview(timestampMs: number): void {
		const cachedPreview = events
			.filter((event) => event.thumbnail_url)
			.toSorted(
				(left, right) =>
					Math.abs(left.start_time_ms - timestampMs) - Math.abs(right.start_time_ms - timestampMs)
			)[0];
		if (cachedPreview && Math.abs(cachedPreview.start_time_ms - timestampMs) <= 30_000) {
			stillPreviewUrl = cachedPreview.thumbnail_url;
		}
	}

	function recordingEventFromHit(hit: EventPreviewHit): RecordingEvent {
		return {
			id: hit.eventId,
			source_id: hit.sourceId,
			revision: hit.revision,
			source: hit.origin,
			kind: hit.eventType,
			start_time_ms: hit.startMs,
			end_time_ms: hit.endMs,
			confidence: hit.confidence,
			bbox: hit.bbox,
			bbox_attachment_id: hit.bboxAttachmentId,
			zone: hit.zone,
			text: hit.text,
			thumbnail_url: null,
			attachments: hit.attachments,
			canonical_attachment_id: hit.canonicalAttachment?.id ?? null,
			icon_key: hit.iconKey,
			rejected_icon_key: hit.rejectedIconKey,
			image_availability: hit.imageAvailability
		};
	}

	async function resolveExportSeedEvent(
		eventId: string,
		sourceId: string,
		timestampMs: number
	): Promise<RecordingEvent | null> {
		const startMs = timestampMs - 5 * 60_000;
		const endMs = timestampMs + 5 * 60_000;
		const pages = await Promise.all(
			(['main', 'sub'] as const).map((streamId) =>
				controlClient
					.searchEventMetadata({
						eventIds: [eventId],
						sourceIds: [sourceId],
						streamId,
						startMs,
						endMs,
						pageSize: 1
					})
					.catch(() => null)
			)
		);
		const hit = pages
			.flatMap((result) => result?.hits ?? [])
			.filter((candidate) => candidate.eventId === eventId && candidate.sourceId === sourceId)
			.toSorted((left, right) => right.revision - left.revision)[0];
		return hit ? recordingEventFromHit(hit) : null;
	}

	function attachKeyFramePreview(playback: StoredMediaPlayback): void {
		keyFrameUnsubscribe?.();
		keyFrameUnsubscribe = playback.onKeyFrame((preview) => {
			void renderStoredKeyFrame(preview);
		});
	}

	async function renderStoredKeyFrame(preview: StoredMediaKeyFramePreview): Promise<void> {
		if (preview.storedMediaId !== storedPlayback?.id) return;
		const version = ++previewVersion;
		const cacheKey = `${cameraId}:${stream}:${preview.timestampMs}:${preview.codec}:${preview.configurationRevision}`;
		const cached = keyframePreviewCache.get(cacheKey);
		if (cached) {
			keyframePreviewCache.delete(cacheKey);
			keyframePreviewCache.set(cacheKey, cached);
			stillPreviewUrl = cached;
			emitTimelinePerformanceEvent('ScrubPreviewRendered', {
				sourceId: cameraId,
				cursorId: preview.storedMediaId,
				generation: String(preview.generation),
				cache: 'memory'
			});
			return;
		}
		try {
			const url = await decodeEventKeyframePreview(preview);
			if (version !== previewVersion || preview.storedMediaId !== storedPlayback?.id) {
				URL.revokeObjectURL(url);
				return;
			}
			keyframePreviewCache.set(cacheKey, url);
			while (keyframePreviewCache.size > 32) {
				const oldest = keyframePreviewCache.entries().next().value as [string, string] | undefined;
				if (!oldest) break;
				keyframePreviewCache.delete(oldest[0]);
				URL.revokeObjectURL(oldest[1]);
			}
			stillPreviewUrl = url;
			emitTimelinePerformanceEvent('ScrubPreviewRendered', {
				sourceId: cameraId,
				cursorId: preview.storedMediaId,
				generation: String(preview.generation),
				cache: 'decoder'
			});
		} catch {
			if (version === previewVersion) {
				stillPreviewUrl = null;
				const playback = storedPlayback;
				if (scrubbing && playback?.id === preview.storedMediaId) {
					scrubUsesFragmentFallback = true;
					await playback.commitPlayback(false, 1).catch(() => undefined);
					playbackUrl = playback.url;
					playbackAnchorMs = playback.anchorTimeMs;
					pendingSeekSeconds = playback.initialOffsetSeconds;
				}
			}
		}
	}

	function queueTimelineScrub(timestampMs: number): Promise<void> {
		scrubTargetMs = timestampMs;
		playheadMs = timestampMs;
		showNearestCachedPreview(timestampMs);
		if (!scrubPump) {
			const version = scrubVersion;
			scrubPump = drainTimelineScrub(version)
				.catch((cause) => {
					if (version !== scrubVersion) return;
					clearColdSeek();
					playerError = storedPlaybackError(cause);
				})
				.finally(() => {
					scrubPump = null;
					if (scrubTargetMs !== null && version === scrubVersion) {
						void queueTimelineScrub(scrubTargetMs);
					}
				});
		}
		return scrubPump;
	}

	async function drainTimelineScrub(version: number): Promise<void> {
		while (scrubTargetMs !== null && version === scrubVersion) {
			const timestampMs = scrubTargetMs;
			scrubTargetMs = null;
			const target = recordingTarget(orderedSegments, timestampMs);
			if (!target) return;
			const requestedTimestampMs = target.segment.start_time_ms + target.offsetSeconds * 1_000;
			const playback = await ensureScrubPlayback(target.segment, requestedTimestampMs, version);
			if (!playback || version !== scrubVersion) return;
			if (scrubTargetMs !== null) continue;
			await playback.seek(requestedTimestampMs);
			if (version !== scrubVersion) return;
			playbackUrl = playback.url;
			playbackAnchorMs = playback.anchorTimeMs;
			pendingSeekSeconds = playback.initialOffsetSeconds;
		}
	}

	async function ensureScrubPlayback(
		segment: RecordingSegment,
		timestampMs: number,
		version: number
	): Promise<StoredMediaPlayback | null> {
		const current = storedPlayback;
		if (
			current &&
			selected?.date === segment.date &&
			current.sourceId === cameraId &&
			current.streamId === segment.stream
		) {
			if (scrubUsesFragmentFallback) await current.commitPlayback(false, 1);
			else await current.enterScrub();
			selected = segment;
			return version === scrubVersion ? current : null;
		}
		const previous = storedPlayback;
		previous?.setPlaying(false);
		scrubOpenController?.abort();
		const openController = new AbortController();
		scrubOpenController = openController;
		let playback: StoredMediaPlayback;
		try {
			playback = await controlClient.openStoredMedia({
				sourceId: cameraId,
				streamId: segment.stream,
				timestampMs,
				endTimeMs: dayStartMs + 86_400_000,
				playing: false,
				playbackRate: 1,
				mode: scrubUsesFragmentFallback ? 'playback' : 'scrub',
				signal: openController.signal
			});
		} catch (cause) {
			if (openController.signal.aborted || version !== scrubVersion) return null;
			throw cause;
		} finally {
			if (scrubOpenController === openController) scrubOpenController = null;
		}
		if (version !== scrubVersion) {
			await playback.close().catch(() => undefined);
			return null;
		}
		storedPlayback = playback;
		selected = segment;
		playbackUrl = playback.url;
		playbackAnchorMs = playback.anchorTimeMs;
		pendingSeekSeconds = playback.initialOffsetSeconds;
		attachKeyFramePreview(playback);
		if (previous && previous !== playback) await previous.close().catch(() => undefined);
		return playback;
	}

	function beginTimelineScrub(timestampMs: number): void {
		scrubVersion += 1;
		scrubOpenController?.abort();
		scrubPlayIntent = playbackIntent();
		scrubbing = true;
		playing = false;
		pauseVideoForTransition();
		void queueTimelineScrub(timestampMs);
	}

	function moveTimelineScrub(timestampMs: number): void {
		void queueTimelineScrub(timestampMs);
	}

	async function finishTimelineScrub(timestampMs: number): Promise<void> {
		await queueTimelineScrub(timestampMs);
		await scrubPump;
		const playback = storedPlayback;
		if (!playback) {
			scrubbing = false;
			return;
		}
		await playback.commitPlayback(scrubPlayIntent, playbackRate);
		playbackUrl = playback.url;
		playbackAnchorMs = playback.anchorTimeMs;
		pendingSeekSeconds = playback.initialOffsetSeconds;
		pendingPlay = scrubPlayIntent;
		playing = scrubPlayIntent;
		scrubbing = false;
		await tick();
		applyPendingSeek();
	}

	function cancelTimelineScrub(): void {
		scrubVersion += 1;
		scrubOpenController?.abort();
		scrubOpenController = null;
		scrubTargetMs = null;
		scrubbing = false;
		playing = scrubPlayIntent;
		void storedPlayback?.commitPlayback(scrubPlayIntent, playbackRate);
		if (scrubPlayIntent) void startReplay();
	}

	async function previewEvent(event: RecordingEvent): Promise<void> {
		const version = ++previewVersion;
		previewController?.abort();
		stillPreviewUrl = event.thumbnail_url;
		if (event.thumbnail_url) {
			return;
		}
		if (!eventSearchAvailable || !cameraId) return;

		const controller = new AbortController();
		previewController = controller;
		try {
			const page = await controlClient.searchEventPreviews({
				sourceId: event.source_id ?? cameraId,
				streamId: stream,
				eventType: event.kind,
				startMs: event.start_time_ms - 60_000,
				endMs: (event.end_time_ms ?? event.start_time_ms) + 60_000,
				signal: controller.signal
			});
			const hit = page.hits.find((candidate) => candidate.eventId === event.id);
			const keyframe = hit?.keyframes
				.filter((candidate) => candidate.byteLength <= 4 * 1_048_576)
				.toSorted(
					(left, right) =>
						Math.abs(left.eventTimeMs - event.start_time_ms) -
						Math.abs(right.eventTimeMs - event.start_time_ms)
				)[0];
			if (!keyframe || version !== previewVersion) return;
			const cacheKey = `${keyframe.sourceId}:${keyframe.streamId}:${keyframe.recordingId}:${keyframe.fragmentSequence}`;
			const cached = keyframePreviewCache.get(cacheKey);
			if (cached) {
				keyframePreviewCache.delete(cacheKey);
				keyframePreviewCache.set(cacheKey, cached);
				stillPreviewUrl = cached;
				return;
			}
			const media = await controlClient.fetchEventPreviewKeyframe(keyframe, controller.signal);
			const url = await decodeEventKeyframePreview(media);
			if (version !== previewVersion || controller.signal.aborted) {
				URL.revokeObjectURL(url);
				return;
			}
			keyframePreviewCache.set(cacheKey, url);
			while (keyframePreviewCache.size > 32) {
				const oldest = keyframePreviewCache.entries().next().value as [string, string] | undefined;
				if (!oldest) break;
				keyframePreviewCache.delete(oldest[0]);
				URL.revokeObjectURL(oldest[1]);
			}
			stillPreviewUrl = url;
		} catch {
			if (version === previewVersion && !controller.signal.aborted) stillPreviewUrl = null;
		}
	}

	function clearStillPreview(): void {
		if (storedPlayback) {
			emitTimelinePerformanceEvent('ReplayFirstFrame', {
				sourceId: cameraId,
				cursorId: storedPlayback.id,
				durationMs:
					coldSeekStartedAt > 0 ? Math.max(0, performance.now() - coldSeekStartedAt) : undefined
			});
		}
		previewVersion += 1;
		previewController?.abort();
		previewController = null;
		stillPreviewUrl = null;
		clearColdSeek();
		releaseSecondaryLoads();
	}

	function handlePlayerLoadedData(): void {
		clearPlaybackStartupTimer();
		playbackStartupPhase = 'first-frame';
		const shouldAnimate = cameraSwitchPending;
		clearStillPreview();
		if (!shouldAnimate) return;
		cameraSwitchPending = false;
		clearCameraSwitchFrame();
		cameraSwitchAnimating = true;
		if (cameraSwitchTimer !== null) window.clearTimeout(cameraSwitchTimer);
		const version = cameraSwitchVersion;
		cameraSwitchTimer = window.setTimeout(() => {
			cameraSwitchTimer = null;
			if (version === cameraSwitchVersion) cameraSwitchAnimating = false;
		}, cameraSwitchDurationMs);
	}

	function clearColdSeek(): void {
		coldSeekTimestampMs = null;
		coldSeekElapsedMs = 0;
		coldSeekStartedAt = 0;
	}

	function clearPlaybackStartupTimer(): void {
		if (playbackStartupTimer === null) return;
		clearTimeout(playbackStartupTimer);
		playbackStartupTimer = null;
	}

	function detachPlaybackObservers(): void {
		playbackErrorUnsubscribe?.();
		playbackErrorUnsubscribe = null;
		playbackStartupUnsubscribe?.();
		playbackStartupUnsubscribe = null;
	}

	function observePlaybackStartup(
		playback: StoredMediaPlayback,
		version: number,
		segment: RecordingSegment,
		timestampMs: number
	): void {
		detachPlaybackObservers();
		playbackErrorUnsubscribe = playback.onError((message) => {
			if (version !== playbackVersion || playback !== storedPlayback) return;
			void handlePlaybackFailure(
				message,
				version,
				selected ?? segment,
				playheadMs ?? timestampMs,
				playbackIntent()
			);
		});
		playbackStartupUnsubscribe = playback.onStartup((event) => {
			if (version !== playbackVersion || playback !== storedPlayback) return;
			playbackStartupPhase = event.phase;
			playbackContentType = event.contentType;
			armPlaybackStartupDeadline(version, segment, timestampMs, event.phase);
		});
	}

	function armPlaybackStartupDeadline(
		version: number,
		segment: RecordingSegment,
		timestampMs: number,
		phase: StoredMediaStartupPhase
	): void {
		clearPlaybackStartupTimer();
		const expected =
			phase === 'metadata'
				? 'recording initialization'
				: phase === 'initialization'
					? 'first media fragment'
					: 'first decoded frame';
		playbackStartupTimer = setTimeout(() => {
			playbackStartupTimer = null;
			if (version !== playbackVersion || playbackStartupPhase === 'first-frame') return;
			void handlePlaybackFailure(
				`No ${expected} arrived within ${startupPhaseTimeoutMs / 1_000} seconds.`,
				version,
				selected ?? segment,
				playheadMs ?? timestampMs,
				playbackIntent()
			);
		}, startupPhaseTimeoutMs);
	}

	async function handlePlaybackFailure(
		message: string,
		version: number,
		failedSegment: RecordingSegment,
		timestampMs: number,
		play: boolean
	): Promise<void> {
		if (version !== playbackVersion || playbackFailureHandling) return;
		playbackFailureHandling = true;
		clearPlaybackStartupTimer();
		clearColdSeek();
		const fallbackStream = fallbackStreams.find((candidate) => candidate !== failedSegment.stream);
		if (!fallbackAttempted && fallbackStream) {
			const candidates = allSegments
				.filter((segment) => segment.stream === fallbackStream)
				.toSorted((left, right) => left.start_time_ms - right.start_time_ms);
			const target = recordingTarget(candidates, timestampMs);
			if (target) {
				fallbackAttempted = true;
				selectedFallbackStream = fallbackStream;
				selectedPlaybackVariant = fallbackStream;
				stream = fallbackStream;
				playbackNotice = `${safePlaybackFailure(message, failedSegment.stream)} Playing ${streamLabel(fallbackStream)} instead.`;
				playbackFailureHandling = false;
				await selectSegment(target.segment, target.offsetSeconds, play);
				updateUrl();
				return;
			}
		}
		cameraSwitchPending = false;
		cameraSwitchAnimating = false;
		playerError = terminalPlaybackFailure(message, failedSegment.stream);
		releaseSecondaryLoads();
		playbackFailureHandling = false;
	}

	function safePlaybackFailure(message: string, failedStream: RecordedStreamId): string {
		if (message.startsWith('Browser does not support ')) return message;
		if (message.startsWith('No ')) return message;
		if (message.includes('timed out')) {
			return `${streamLabel(failedStream)} playback did not open within 4 seconds.`;
		}
		return `${streamLabel(failedStream)} playback failed before its first frame.`;
	}

	function terminalPlaybackFailure(message: string, failedStream: RecordedStreamId): string {
		const detail = safePlaybackFailure(message, failedStream);
		const fallbackDetail = fallbackAttempted ? ' The compatible fallback also failed.' : '';
		return `${detail}${fallbackDetail} Retry the recording or configure an H.264 recording profile.`;
	}

	function deferSecondaryLoads(): void {
		secondaryLoadsReady = false;
		scheduleSecondaryLoads();
	}

	function scheduleSecondaryLoads(): void {
		if (secondaryLoadsReady) return;
		if (secondaryLoadsTimer !== null) window.clearTimeout(secondaryLoadsTimer);
		secondaryLoadsTimer = window.setTimeout(releaseSecondaryLoads, 5_000);
	}

	function releaseSecondaryLoads(): void {
		if (secondaryLoadsTimer !== null) window.clearTimeout(secondaryLoadsTimer);
		secondaryLoadsTimer = null;
		if (secondaryLoadsReady) return;
		secondaryLoadsReady = true;
		if (mode === 'timeline' && latestTimelineViewport) {
			void loadTimelineViewport(latestTimelineViewport);
		}
	}

	function cameraDirection(nextCameraId: string): -1 | 1 {
		const currentIndex = cameras.findIndex((camera) => camera.id === cameraId);
		const nextIndex = cameras.findIndex((camera) => camera.id === nextCameraId);
		return currentIndex >= 0 && nextIndex < currentIndex ? -1 : 1;
	}

	function beginCameraSwitch(direction: -1 | 1): void {
		cameraSwitchDirection = direction;
		cameraSwitchPending = true;
		cameraSwitchAnimating = false;
		cameraSwitchVersion += 1;
		if (cameraSwitchTimer !== null) window.clearTimeout(cameraSwitchTimer);
		cameraSwitchTimer = null;
		if (!cameraSwitchFrameUrl && video) {
			void captureCameraSwitchFrame(video, cameraSwitchVersion);
		}
	}

	async function captureCameraSwitchFrame(
		element: HTMLVideoElement,
		version: number
	): Promise<void> {
		if (element.videoWidth <= 0 || element.videoHeight <= 0) return;
		const scale = Math.min(1, 960 / Math.max(element.videoWidth, element.videoHeight));
		const canvas = document.createElement('canvas');
		canvas.width = Math.max(1, Math.round(element.videoWidth * scale));
		canvas.height = Math.max(1, Math.round(element.videoHeight * scale));
		const context = canvas.getContext('2d');
		if (!context) return;
		context.drawImage(element, 0, 0, canvas.width, canvas.height);
		const blob = await new Promise<Blob | null>((resolve) =>
			canvas.toBlob(resolve, 'image/jpeg', 0.82)
		);
		if (!blob || version !== cameraSwitchVersion || !cameraSwitchPending) return;
		const url = URL.createObjectURL(blob);
		if (cameraSwitchFrameUrl) URL.revokeObjectURL(cameraSwitchFrameUrl);
		cameraSwitchFrameUrl = url;
	}

	function clearCameraSwitchFrame(): void {
		if (!cameraSwitchFrameUrl) return;
		URL.revokeObjectURL(cameraSwitchFrameUrl);
		cameraSwitchFrameUrl = null;
	}

	function selectCamera(
		nextCameraId: string,
		direction: -1 | 1,
		timestampMs = playheadMs ?? undefined
	): void {
		if (nextCameraId === cameraId) return;
		const play = playbackIntent();
		beginCameraSwitch(direction);
		timelineRepository.deactivate();
		latestTimelineViewport = null;
		cameraId = nextCameraId;
		void loadRecordings(selectedDate || undefined, timestampMs, play).then(
			scheduleRecordingDateDiscovery
		);
	}

	function openTimestamp(timestampMs: number): void {
		mode = 'timeline';
		seekToTimestamp(timestampMs, playbackIntent());
		updateUrl();
	}

	function selectSwimlane(nextCameraId: string, timestampMs: number): void {
		mode = 'timeline';
		if (nextCameraId !== cameraId) {
			selectCamera(nextCameraId, cameraDirection(nextCameraId), timestampMs);
			return;
		}
		void loadRecordings(selectedDate || undefined, timestampMs, playbackIntent()).then(
			scheduleRecordingDateDiscovery
		);
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
		latestTimelineViewport = null;
		void loadRecordings(date, undefined, playbackIntent());
	}

	function changeRecordedQuality(next: RecordedQualityPreference): void {
		const selection = chooseRecordedStream(allSegments, null, next);
		if (selection.selectedStream === null) {
			playerError = unsupportedRecordedPlaybackMessage(selection);
			return;
		}
		if ((next === 'main' || next === 'sub') && selection.selectedStream !== next) {
			playerError = null;
			return;
		}
		playbackPreferences = withRecordedPreference(playbackPreferences, cameraId, next);
		savePlaybackPreferences(window.localStorage, playbackPreferences);
		if (selection.selectedStream === stream && selected !== null) return;
		const targetTimestampMs = playheadMs;
		stream = selection.selectedStream;
		const candidates = allSegments
			.filter((segment) => segment.stream === stream)
			.toSorted((left, right) => left.start_time_ms - right.start_time_ms);
		const target =
			targetTimestampMs === null ? null : recordingTarget(candidates, targetTimestampMs);
		void selectSegment(
			target?.segment ?? candidates.at(-1) ?? null,
			target?.offsetSeconds ?? 0,
			playbackIntent()
		);
		updateUrl();
	}

	function handleRecordedQualityChange(event: Event): void {
		const target = event.currentTarget;
		if (!(target instanceof HTMLSelectElement)) return;
		const option = recordedQualityOptions.find((candidate) => candidate.value === target.value);
		if (option) changeRecordedQuality(option.value);
	}

	function handlePlayerError(event: Event) {
		const media = event.currentTarget;
		const segment = selected;
		if (!(media instanceof HTMLVideoElement) || !segment) return;
		const message = media.error?.message || 'The browser rejected stored playback.';
		void handlePlaybackFailure(
			message,
			playbackVersion,
			segment,
			playheadMs ?? segment.start_time_ms,
			pendingPlay || playing
		);
	}

	function recordingSegment(
		streamId: 'main' | 'sub',
		date: string,
		startTimeMs: number,
		endTimeMs: number
	): RecordingSegment {
		return {
			stream: streamId,
			date,
			hour: new Date(startTimeMs).toISOString().slice(11, 13),
			filename: `${startTimeMs}-${endTimeMs}.mp4`,
			url: `stored:${cameraId}:${streamId}:${startTimeMs}:${endTimeMs}`,
			start_time_ms: startTimeMs,
			end_time_ms: endTimeMs,
			duration_ms: endTimeMs - startTimeMs
		};
	}

	function mergeRecordingSegments(candidates: readonly RecordingSegment[]): RecordingSegment[] {
		const merged = new Map<string, RecordingSegment>();
		for (const segment of candidates) {
			merged.set(`${segment.stream}:${segment.start_time_ms}:${segment.end_time_ms}`, segment);
		}
		return [...merged.values()];
	}

	async function selectSegment(segment: RecordingSegment | null, offsetSeconds = 0, play = false) {
		if (!segment) {
			cameraSwitchPending = false;
			cameraSwitchAnimating = false;
			clearCameraSwitchFrame();
			playbackVersion += 1;
			playbackOpenController?.abort();
			playbackOpenController = null;
			clearPlaybackStartupTimer();
			detachPlaybackObservers();
			playbackStartupPhase = 'idle';
			await closeStoredPlayback();
			selected = null;
			playheadMs = null;
			playing = false;
			releaseSecondaryLoads();
			return;
		}
		const requestedTimestampMs =
			segment.start_time_ms +
			Math.max(0, Math.min(offsetSeconds, Math.max(0, segment.duration_ms / 1_000 - 0.001))) *
				1_000;
		const sameSegment = selected?.url === segment.url && storedPlayback !== null;
		const reusablePlayback =
			storedPlayback !== null &&
			selected?.date === segment.date &&
			storedPlayback.sourceId === cameraId &&
			storedPlayback.streamId === segment.stream;
		if (selected?.url !== segment.url) {
			exportRangeStartMs = null;
			exportRangeEndMs = null;
		}
		const requestedOffsetSeconds = (requestedTimestampMs - segment.start_time_ms) / 1_000;
		playerError = null;
		if (sameSegment) {
			const playback = storedPlayback;
			if (!playback) return;
			const canSeekLocally = playback.canSeekLocally(requestedTimestampMs);
			selected = segment;
			pendingPlay = play;
			playing = play;
			playheadMs = requestedTimestampMs;
			pendingSeekSeconds = Math.max(0, (playheadMs - playbackAnchorMs) / 1_000);
			await tick();
			applyPendingSeek();
			if (!canSeekLocally) {
				const version = ++playbackVersion;
				coldSeekTimestampMs = requestedTimestampMs;
				coldSeekElapsedMs = 0;
				coldSeekStartedAt = performance.now();
				try {
					await playback.seek(requestedTimestampMs);
				} catch (cause) {
					clearColdSeek();
					if (version === playbackVersion) {
						playerError = storedPlaybackError(cause);
					}
					return;
				}
				if (version !== playbackVersion || playback !== storedPlayback) return;
				playbackUrl = playback.url;
				playbackAnchorMs = playback.anchorTimeMs;
				pendingSeekSeconds = playback.initialOffsetSeconds;
			}
			return;
		}
		const version = ++playbackVersion;
		const previousPlayback = storedPlayback;
		if (previousPlayback) {
			pauseVideoForTransition();
			previousPlayback.setPlaying(false);
			coldSeekTimestampMs = requestedTimestampMs;
			coldSeekElapsedMs = 0;
			coldSeekStartedAt = performance.now();
		}
		if (reusablePlayback && previousPlayback) {
			try {
				await previousPlayback.seek(requestedTimestampMs);
			} catch (cause) {
				clearColdSeek();
				if (version === playbackVersion) {
					playerError = storedPlaybackError(cause);
				}
				return;
			}
			if (version !== playbackVersion) return;
			selected = segment;
			pendingPlay = play;
			playing = play;
			playheadMs = requestedTimestampMs;
			playbackUrl = previousPlayback.url;
			playbackAnchorMs = previousPlayback.anchorTimeMs;
			pendingSeekSeconds = previousPlayback.initialOffsetSeconds;
			previousPlayback.setPlaybackRate(playbackRate);
			previousPlayback.setPlaying(play);
			await tick();
			applyPendingSeek();
			return;
		}
		let playback: StoredMediaPlayback;
		playbackOpenController?.abort();
		const openController = new AbortController();
		playbackOpenController = openController;
		playbackStartupPhase = 'opening';
		playbackContentType = null;
		try {
			playback = await controlClient.openStoredMedia({
				sourceId: cameraId,
				streamId: segment.stream,
				timestampMs: requestedTimestampMs,
				endTimeMs: dayStartMs + 86_400_000,
				playing: play,
				playbackRate,
				signal: openController.signal
			});
		} catch (cause) {
			clearColdSeek();
			if (openController.signal.aborted || version !== playbackVersion) return;
			await handlePlaybackFailure(
				storedPlaybackError(cause),
				version,
				segment,
				requestedTimestampMs,
				play
			);
			return;
		} finally {
			if (playbackOpenController === openController) playbackOpenController = null;
		}
		if (version !== playbackVersion) {
			await playback.close().catch(() => undefined);
			return;
		}
		selected = segment;
		pendingPlay = play;
		playing = play;
		playheadMs = requestedTimestampMs;
		storedPlayback = playback;
		attachKeyFramePreview(playback);
		observePlaybackStartup(playback, version, segment, requestedTimestampMs);
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
		playbackOpenController?.abort();
		playbackOpenController = null;
		scrubOpenController?.abort();
		scrubOpenController = null;
		clearPlaybackStartupTimer();
		detachPlaybackObservers();
		keyFrameUnsubscribe?.();
		keyFrameUnsubscribe = null;
		storedPlayback = null;
		scrubUsesFragmentFallback = false;
		playbackUrl = null;
		if (playback) {
			await tick();
			await playback.close().catch(() => undefined);
		}
	}

	function seekToTimestamp(timestampMs: number, play = true) {
		showNearestCachedPreview(timestampMs);
		if (recordingCoverage.some((range) => timestampInRange(timestampMs, range))) {
			targetLoadVersion += 1;
			targetLoadController?.abort();
			const target = exactRecordingTarget(timestampMs);
			if (target) {
				void selectSegment(target.segment, target.offsetSeconds, play);
			} else {
				clearColdSeek();
				playerError = 'No indexed footage is available near that time.';
			}
			return;
		}
		void loadExactTarget(timestampMs, play);
	}

	async function loadExactTarget(timestampMs: number, play: boolean): Promise<void> {
		if (!cameraId || !selectedDate) return;
		const version = ++targetLoadVersion;
		targetLoadController?.abort();
		const controller = new AbortController();
		targetLoadController = controller;
		const sourceId = cameraId;
		const streamId = stream;
		const date = selectedDate;
		const startMs = Math.max(dayStartMs, timestampMs - initialRecordingWindowMs);
		const endMs = Math.min(dayStartMs + 86_400_000, timestampMs + initialRecordingWindowMs);
		try {
			const [response] = await controlClient.getRecordingsInRange(
				[sourceId],
				date,
				startMs,
				endMs,
				controller.signal
			);
			if (
				version !== targetLoadVersion ||
				controller.signal.aborted ||
				sourceId !== cameraId ||
				streamId !== stream ||
				date !== selectedDate
			) {
				return;
			}
			segments = mergeRecordingSegments([...segments, ...response.segments]);
			recordingCoverage = [...recordingCoverage, { startMs, endMs }];
			const target = recordingTarget(
				response.segments
					.filter((segment) => segment.stream === streamId)
					.toSorted((left, right) => left.start_time_ms - right.start_time_ms),
				timestampMs
			);
			if (target) {
				await selectSegment(target.segment, target.offsetSeconds, play);
			} else {
				clearColdSeek();
				playerError = 'No indexed footage is available near that time.';
			}
		} catch (cause) {
			if (controller.signal.aborted || version !== targetLoadVersion) return;
			clearColdSeek();
			playerError = storedPlaybackError(cause);
		} finally {
			if (targetLoadController === controller) targetLoadController = null;
		}
	}

	function timestampInRange(timestampMs: number, range: TimelineInterval): boolean {
		return timestampMs >= range.startMs && timestampMs < range.endMs;
	}

	function exactRecordingTarget(timestampMs: number) {
		const coverage = recordingCoverage.findLast((range) => timestampInRange(timestampMs, range));
		if (!coverage) return null;
		return recordingTarget(
			playableSegments.filter(
				(segment) =>
					segment.start_time_ms < coverage.endMs && segment.end_time_ms > coverage.startMs
			),
			timestampMs
		);
	}

	function storedPlaybackError(cause: unknown): string {
		if (cause instanceof Error && cause.message.includes('timestamp is unavailable')) {
			return 'No indexed footage is available at that exact time.';
		}
		return cause instanceof Error ? cause.message : 'This recording could not be opened.';
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
		playbackPreferences = withMediaPreferences(playbackPreferences, { playbackRate: speed });
		savePlaybackPreferences(window.localStorage, playbackPreferences);
		if (video) video.playbackRate = speed;
		storedPlayback?.setPlaybackRate(speed);
	}

	function setPlaying(nextPlaying: boolean): void {
		playing = nextPlaying;
		rememberPlayIntent(nextPlaying);
		storedPlayback?.setPlaying(nextPlaying);
		if (!video) {
			pendingPlay = nextPlaying;
			return;
		}
		if (nextPlaying) {
			void startReplay();
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
		video.muted = scrubbing || playbackMuted;
		const requestedTime = Math.max(
			0,
			Math.min(pendingSeekSeconds, video.duration || pendingSeekSeconds)
		);
		video.currentTime = requestedTime;
		if (Math.abs(video.currentTime - requestedTime) < 0.001) {
			playheadMs = playbackAnchorMs + video.currentTime * 1_000;
		}
		storedPlayback?.observe(video.currentTime);
		if (pendingPlay) void startReplay();
		pendingPlay = false;
	}

	async function startReplay(): Promise<void> {
		const player = video;
		if (!player) return;
		try {
			await player.play();
		} catch {
			if (video !== player) return;
			player.muted = true;
			playbackMuted = true;
			playbackPreferences = withMediaPreferences(playbackPreferences, { muted: true });
			savePlaybackPreferences(window.localStorage, playbackPreferences);
			await player.play().catch(() => (playing = false));
		}
	}

	function updatePlayhead() {
		if (!video || !selected) return;
		playheadMs = playbackAnchorMs + video.currentTime * 1_000;
		storedPlayback?.observe(video.currentTime);
	}

	function updatePlaybackRate() {
		if (!video) return;
		playbackRate = video.playbackRate;
		playbackPreferences = withMediaPreferences(playbackPreferences, { playbackRate });
		savePlaybackPreferences(window.localStorage, playbackPreferences);
		storedPlayback?.setPlaybackRate(playbackRate);
	}

	function updateMutedPreference(): void {
		if (!video || scrubbing || video.muted === playbackMuted) return;
		playbackMuted = video.muted;
		playbackPreferences = withMediaPreferences(playbackPreferences, { muted: playbackMuted });
		savePlaybackPreferences(window.localStorage, playbackPreferences);
	}

	function playbackIntent(): boolean {
		if (pendingPlay || playing || (video !== null && !video.paused)) return true;
		return storedPlayback === null && playbackPreferences.media.playing;
	}

	function handlePlay() {
		playing = true;
		rememberPlayIntent(true);
		shuttleDirection = 1;
		storedPlayback?.setPlaying(true);
	}

	function handlePause() {
		if (ignoreNextPauseEvent) {
			ignoreNextPauseEvent = false;
			return;
		}
		playing = false;
		rememberPlayIntent(false);
		if (shuttleDirection === 1) shuttleDirection = 0;
		storedPlayback?.setPlaying(false);
	}

	function rememberPlayIntent(nextPlaying: boolean): void {
		if (playbackPreferences.media.playing === nextPlaying) return;
		playbackPreferences = withMediaPreferences(playbackPreferences, { playing: nextPlaying });
		savePlaybackPreferences(window.localStorage, playbackPreferences);
	}

	function pauseVideoForTransition(): void {
		if (!video || video.paused) return;
		ignoreNextPauseEvent = true;
		video.pause();
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
			...(mode === 'timeline' ? {} : { mode }),
			...(exportSeedEvent?.source_id === cameraId
				? {
						event: exportSeedEvent.id,
						at: String(exportSeedEvent.start_time_ms)
					}
				: {})
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
						class="h-11 rounded-xs px-2.5 text-2xs font-semibold md:h-7 {mode === nextMode
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
			<KeepCameraSwitcher
				{cameras}
				selectedCameraId={cameraId}
				switching={cameraSwitchPending}
				onselect={selectCamera}
			/>

			<div class="grid gap-1">
				<span class="text-xs font-medium text-muted-foreground">Date</span>
				<div class="flex items-center rounded-md border bg-background">
					<Button
						variant="ghost"
						size="icon"
						class="size-11 md:size-9"
						title="Previous recorded day"
						disabled={!olderDate}
						onclick={() => olderDate && changeDate(olderDate)}
					>
						<ChevronLeftIcon />
					</Button>
					<label class="relative flex h-11 items-center gap-2 border-x px-2 md:h-9">
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
						class="size-11 md:size-9"
						title="Next recorded day"
						disabled={!newerDate}
						onclick={() => newerDate && changeDate(newerDate)}
					>
						<ChevronRightIcon />
					</Button>
				</div>
			</div>

			{#if availableStreams.size > 0}
				<div class="grid gap-1">
					<label for="recorded-quality" class="text-xs font-medium text-muted-foreground">
						Quality
					</label>
					<select
						id="recorded-quality"
						value={recordedPreference(playbackPreferences, cameraId)}
						class="h-11 rounded-md border bg-background px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring md:h-9"
						onchange={handleRecordedQualityChange}
					>
						{#each recordedQualityOptions as option (option.value)}
							<option value={option.value}>{option.label}</option>
						{/each}
					</select>
				</div>
			{/if}

			<Button
				variant="outline"
				size="icon"
				class="size-11 md:size-9"
				title="Refresh recordings"
				disabled={!cameraId || loading}
				onclick={() => void loadRecordings(selectedDate || undefined, undefined, playbackIntent())}
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

	{#if loading && segments.length === 0 && selected === null}
		<div class="grid gap-4 lg:grid-cols-[minmax(0,1fr)_24.75rem]">
			<Skeleton class="aspect-video w-full rounded-md" />
			<Skeleton class="h-[34rem] w-full rounded-md" />
		</div>
	{:else if mode === 'stories'}
		<KeepStories
			events={storyModeEvents}
			{dates}
			{selectedDate}
			ondate={changeDate}
			onseek={openTimestamp}
		/>
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
		{#key `${selected?.url ?? 'empty-export'}:${exportSeedEvent?.id ?? ''}:${exportSeedEvent?.revision ?? ''}`}
			<KeepExportPanel
				sourceId={cameraId}
				sourceName={selectedCamera?.name ?? cameraId}
				segment={selected}
				bitrateKbps={selectedBitrateKbps}
				rangeStartMs={exportRangeStartMs}
				rangeEndMs={exportRangeEndMs}
				event={exportSeedEvent}
			/>
		{/key}
	{:else}
		<div class="grid min-h-0 items-start gap-4 lg:grid-cols-[minmax(0,1fr)_24.75rem]">
			<section
				data-keep-player
				data-recording-requested-variant={requestedPlaybackVariant}
				data-recording-selected-variant={selectedPlaybackVariant ?? undefined}
				data-recording-fallback-variant={selectedFallbackStream ?? undefined}
				data-recording-selection-reason={selectedPlaybackReason}
				data-recording-startup-phase={playbackStartupPhase}
				data-recording-content-type={playbackContentType ?? undefined}
				data-recording-rejected-variants={rejectedStreams
					.map((candidate) => `${candidate.stream}:${candidate.encoding}`)
					.join(',') || undefined}
				data-keyboard-shuttle-direction={shuttleDirection}
				data-keyboard-shuttle-speed={shuttleSpeed}
				data-keyboard-playing={playing}
				data-recording-playhead-ms={playheadMs}
				data-camera-transition={cameraSwitchPending
					? 'loading'
					: cameraSwitchAnimating
						? 'entering'
						: 'idle'}
				data-camera-transition-direction={cameraSwitchDirection === 1 ? 'next' : 'previous'}
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
								muted={scrubbing || playbackMuted}
								preload="metadata"
								src={playbackUrl}
								class="aspect-video w-full object-contain {cameraSwitchAnimating
									? cameraSwitchDirection === 1
										? 'camera-switch-enter-next'
										: 'camera-switch-enter-previous'
									: ''}"
								onloadedmetadata={applyPendingSeek}
								ondurationchange={applyPendingSeek}
								onloadeddata={handlePlayerLoadedData}
								onseeked={clearStillPreview}
								ontimeupdate={updatePlayhead}
								onended={handleEnded}
								onplay={handlePlay}
								onpause={handlePause}
								onratechange={updatePlaybackRate}
								onvolumechange={updateMutedPreference}
								onerror={handlePlayerError}
							></video>
						{/key}
						{#if stillPreviewUrl}
							<img
								src={stillPreviewUrl}
								alt=""
								class="pointer-events-none absolute inset-0 z-10 size-full bg-black object-contain"
							/>
						{/if}
						{#if cameraSwitchFrameUrl}
							<img
								src={cameraSwitchFrameUrl}
								alt=""
								data-camera-switch-frame
								class="pointer-events-none absolute inset-0 z-10 size-full bg-black object-contain"
							/>
						{/if}
						<span
							data-camera-name
							class="pointer-events-none absolute top-3 right-3 z-20 max-w-[calc(100%-1.5rem)] truncate rounded-sm bg-black/72 px-2 py-1 text-xs font-semibold text-white shadow-sm backdrop-blur-sm"
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
							class="absolute inset-0 z-30"
						/>
					{/if}
				</div>

				{#if playbackNotice}
					<p class="text-sm text-amber-700 dark:text-amber-300" role="status">
						{playbackNotice}
					</p>
				{/if}

				{#if playerError}
					<p class="text-sm text-destructive" role="alert">{playerError}</p>
				{/if}

				<div
					class="flex min-h-10 items-center justify-end gap-1 border-b pb-3"
					aria-label="Playback controls"
				>
					<Button
						variant="outline"
						size="icon-sm"
						class="size-11 md:size-8"
						title="Back 10 seconds"
						disabled={!selected}
						onclick={() => skip(-10)}
					>
						<RotateCcwIcon />
					</Button>
					<Button
						variant="outline"
						size="icon-sm"
						class="size-11 md:size-8"
						title="Forward 10 seconds"
						disabled={!selected}
						onclick={() => skip(10)}
					>
						<RotateCwIcon />
					</Button>
				</div>
			</section>

			{#if mobilePortrait}
				<HorizontalTimeline
					segments={orderedSegments}
					{events}
					selectedUrl={selected?.url ?? null}
					{playheadMs}
					{dayStartMs}
					followRequest={timelineFollowRequest}
					loading={timelineRepository.loading}
					onSeek={seekToTimestamp}
					onEventPreview={(event) => void previewEvent(event)}
					onScrubStart={beginTimelineScrub}
					onScrub={moveTimelineScrub}
					onScrubEnd={(timestampMs) => void finishTimelineScrub(timestampMs)}
					onScrubCancel={cancelTimelineScrub}
					onViewportChange={handleTimelineViewport}
				/>
			{:else}
				<VerticalTimeline
					segments={orderedSegments}
					{events}
					selectedUrl={selected?.url ?? null}
					{playheadMs}
					{dayStartMs}
					followRequest={timelineFollowRequest}
					loading={timelineRepository.loading}
					onSeek={seekToTimestamp}
					onEventPreview={(event) => void previewEvent(event)}
					onScrubStart={beginTimelineScrub}
					onScrub={moveTimelineScrub}
					onScrubEnd={(timestampMs) => void finishTimelineScrub(timestampMs)}
					onScrubCancel={cancelTimelineScrub}
					onViewportChange={handleTimelineViewport}
				/>
			{/if}
		</div>
	{/if}
</div>

<style>
	.camera-switch-enter-next {
		animation: camera-switch-enter-next 180ms cubic-bezier(0.22, 1, 0.36, 1) both;
	}

	.camera-switch-enter-previous {
		animation: camera-switch-enter-previous 180ms cubic-bezier(0.22, 1, 0.36, 1) both;
	}

	@keyframes camera-switch-enter-next {
		from {
			opacity: 0;
			transform: translate3d(8px, 0, 0);
		}
		to {
			opacity: 1;
			transform: translate3d(0, 0, 0);
		}
	}

	@keyframes camera-switch-enter-previous {
		from {
			opacity: 0;
			transform: translate3d(-8px, 0, 0);
		}
		to {
			opacity: 1;
			transform: translate3d(0, 0, 0);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.camera-switch-enter-next,
		.camera-switch-enter-previous {
			animation-name: camera-switch-enter-fade;
			animation-duration: 120ms;
		}

		@keyframes camera-switch-enter-fade {
			from {
				opacity: 0;
			}
			to {
				opacity: 1;
			}
		}
	}
</style>
