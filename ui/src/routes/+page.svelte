<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto, onNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { onMount, tick } from 'svelte';
	import type { CameraHealth, CameraListItem, LiveQuality } from '$lib/types';
	import { useControlClient } from '$lib/control-context';
	import { usePeekViewState } from '$lib/peek-view-context.svelte';
	import { useLivePeer } from '$lib/stream-peer-context';
	import { useShellHealthPublisher } from '$lib/shell-health-context';
	import type { LivePeerPlan } from '$lib/stream-peer.svelte';
	import type { GridTileVisibility } from '$lib/grid-visibility';
	import { emitTimelinePerformanceEvent } from '$lib/timeline-observability';
	import { videoResolutionMatches } from '$lib/video-resolution';
	import {
		GridStreamScheduler,
		type GridTileDemand,
		webDecoderBudget
	} from '$lib/grid-stream-scheduler';
	import LiveVideo from '$lib/components/LiveVideo.svelte';
	import PeekCameraTile from '$lib/components/PeekCameraTile.svelte';
	import PeekDashboardSwitcher from '$lib/components/PeekDashboardSwitcher.svelte';
	import {
		peekCameraStateColorClass,
		presentPeekCamera,
		presentPeekRecordingDiagnostics
	} from '$lib/peek-camera';
	import { isKeyboardTypingTarget } from '$lib/keyboard-shortcuts';
	import { selectPeekLayout, type PeekLayout } from '$lib/peek-layout';
	import { browserSupportsLiveEncoding, selectRecordedStream } from '$lib/recorded-playback-policy';
	import {
		defaultPlaybackPreferences,
		focusedLivePreference,
		loadPlaybackPreferences,
		savePlaybackPreferences,
		withFocusedLivePreference,
		type FocusedLivePreference
	} from '$lib/playback-preferences';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import GaugeIcon from '@lucide/svelte/icons/gauge';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import RadioIcon from '@lucide/svelte/icons/radio';

	type Props = {
		view?: 'dashboard' | 'viewer';
	};

	let { view = 'dashboard' }: Props = $props();

	const qualityOptions: ReadonlyArray<{ value: FocusedLivePreference; label: string }> = [
		{ value: 'auto', label: 'Auto' },
		{ value: 'high', label: 'High' },
		{ value: 'low', label: 'Low' },
		{ value: 'main', label: 'Main' },
		{ value: 'sub', label: 'Sub' }
	];
	const wallRevealTimeoutMs = 5_000;
	const healthRefreshIntervalMs = 5_000;
	const backgroundWarmDurationMs = 5 * 60 * 1_000;
	const backgroundPulseIntervalMs = 1_000;
	const backgroundPulseMaximumDurationMs = 750;
	const transitionFrameMaxDimension = 1_280;
	const cameraFrameMaxDimension = 640;
	const lastViewerCameraKey = 'keeppeek.viewer.last-camera';
	const controlClient = useControlClient();
	const peekViewState = usePeekViewState();
	const livePeer = useLivePeer();
	const publishShellHealth = useShellHealthPublisher();
	const gridScheduler = new GridStreamScheduler({ subscriptionSlots: 4, decoderSlots: 4 });
	const legacyRootCameraId =
		page.url.pathname === resolve('/') ? (page.url.searchParams.get('camera')?.trim() ?? '') : '';
	const initialRequestedCameraId = page.url.searchParams.get('camera')?.trim() ?? '';

	let serverHealth = $derived(peekViewState.serverHealth);
	let cameras = $derived(peekViewState.cameras);
	let error = $derived(peekViewState.error);
	let loading = $derived(!peekViewState.loaded);
	let layoutRegistry = $derived(peekViewState.layoutRegistry);
	let layoutError = $derived(peekViewState.layoutError);
	let transition = $derived(peekViewState.transition);
	let layoutSaving = $state(false);
	let focusedCameraId: string | null = $state(null);
	let lastViewerCameraId = '';
	let viewerSelectionReady = $state(initialRequestedCameraId.length > 0);
	let cameraViewActive = $derived(view === 'viewer');
	let requestedCameraId = $derived(page.url.searchParams.get('camera')?.trim() ?? '');
	let focusQuality = $state<FocusedLivePreference>('auto');
	let playbackPreferences = $state.raw(defaultPlaybackPreferences());
	let focusFallbackVariant = $state<'main' | 'sub' | null>(null);
	let focusFallbackAttempted = false;
	let focusPreviewPresented = $state(false);
	let focusRuntimeNotice = $state<string | null>(null);
	let livePlans = $state.raw<LivePeerPlan[]>([]);
	let livePlansReady = $state(false);
	let tileVisibility = $state.raw<Record<string, GridTileVisibility>>({});
	let componentActive = true;
	let screenActive = true;
	let schedulerTimer: number | null = null;
	let livePlanReconcileScheduled = false;
	let decoderCapacity = 4;
	let backgroundStreamRate = $state<'full' | '1fps'>('full');
	let backgroundPulseCameraIds = $state.raw<ReadonlySet<string>>(new Set());
	let backgroundWarmTimer: ReturnType<typeof setTimeout> | null = null;
	let backgroundPulseTimer: ReturnType<typeof setInterval> | null = null;
	let backgroundPulseEndTimer: ReturnType<typeof setTimeout> | null = null;
	let wallFrameCameraIds = $state.raw<ReadonlySet<string>>(new Set());
	let wallTargetCameraIds = $state.raw<readonly string[]>([]);
	let wallRevealState = $state<'staging' | 'cached' | 'frames' | 'timeout'>(
		peekViewState.wallRevealed ? 'frames' : 'staging'
	);
	let wallRevealTimer: ReturnType<typeof setTimeout> | null = null;
	let healthRefreshInFlight = false;
	let focusReturnPending = $state(false);
	let wallRevealed = $derived(wallRevealState !== 'staging');
	let activeLayout = $derived<PeekLayout | null>(
		layoutRegistry?.layouts.find((layout) => layout.id === layoutRegistry?.activeLayoutId) ?? null
	);
	let focusedCamera = $derived(
		focusedCameraId === null
			? null
			: (cameras.find((camera) => camera.id === focusedCameraId) ?? null)
	);
	let focusedTrack = $derived(focusedCameraId === null ? null : livePeer.track(focusedCameraId));
	let focusedVariantSelection = $derived.by(() => {
		if (!focusedCamera) return null;
		return selectRecordedStream(focusedCamera, {
			requestedStream: focusQuality === 'main' || focusQuality === 'sub' ? focusQuality : null,
			preference: focusQuality,
			isEncodingSupported: browserSupportsLiveEncoding
		});
	});
	let focusedVariant = $derived(
		focusFallbackVariant ?? focusedVariantSelection?.selectedStream ?? 'main'
	);
	let effectiveFocusQuality = $derived<LiveQuality>(
		focusQuality === 'main' || focusQuality === 'sub' ? 'auto' : focusQuality
	);
	let focusCompatibilityNotice = $derived.by(() => {
		const selection = focusedVariantSelection;
		if (!selection || selection.selectedStream === null) return null;
		const rejected = selection.rejectedStreams.find(
			(candidate) => candidate.stream === focusQuality
		);
		if (!rejected) return null;
		return `${focusStreamLabel(rejected.stream)} uses ${rejected.encoding}, which this browser cannot decode. Showing ${focusStreamLabel(selection.selectedStream)} instead.`;
	});
	let focusNotice = $derived(focusRuntimeNotice ?? focusCompatibilityNotice);
	let cameraHealthById = $derived(
		new Map((serverHealth?.cameras ?? []).map((camera) => [camera.id, camera]))
	);
	let focusedRecordingDiagnostics = $derived(
		presentPeekRecordingDiagnostics(
			focusedCamera === null ? null : (cameraHealthById.get(focusedCamera.id) ?? null)
		)
	);
	let focusedDiagnosticsStatusClass = $derived(
		peekCameraStateColorClass(
			focusedCamera === null
				? 'unknown'
				: (cameraHealthById.get(focusedCamera.id)?.state ?? 'unknown')
		)
	);

	onNavigate(async ({ from, to }) => {
		const dashboardPath = resolve('/');
		const viewerPath = resolve('/viewer');
		if (!to) {
			stopBackgroundCadence();
			const currentTransition = peekViewState.transition;
			if (currentTransition) peekViewState.finishTransition(currentTransition);
			return;
		}
		const destinationPath = to.url.pathname;
		if (destinationPath !== dashboardPath && destinationPath !== viewerPath) {
			stopBackgroundCadence();
			const currentTransition = peekViewState.transition;
			if (currentTransition) peekViewState.finishTransition(currentTransition);
			return;
		}
		const sourcePath = from?.url.pathname;
		if (sourcePath !== dashboardPath && sourcePath !== viewerPath) {
			const currentTransition = peekViewState.transition;
			if (currentTransition) peekViewState.finishTransition(currentTransition);
			return;
		}
		const destinationCameraId = to.url.searchParams.get('camera')?.trim() || null;
		const sourceCameraId = from?.url.searchParams.get('camera')?.trim() || null;
		if (
			sourcePath === destinationPath &&
			(destinationPath !== viewerPath || sourceCameraId === destinationCameraId)
		) {
			return;
		}
		if (destinationPath === viewerPath) {
			const currentTransition = peekViewState.transition;
			if (currentTransition) peekViewState.finishTransition(currentTransition);
			if (destinationCameraId) {
				const frame = captureCameraFrame(destinationCameraId);
				if (frame) peekViewState.updateCameraFrames({ [destinationCameraId]: frame });
			}
			return;
		}
		if (
			sourcePath === viewerPath &&
			destinationPath === dashboardPath &&
			wallRevealState !== 'staging'
		) {
			const currentTransition = peekViewState.transition;
			if (currentTransition) peekViewState.finishTransition(currentTransition);
			finishFocusReturn();
			return;
		}
		const cameraFrames = captureCameraFrames();
		peekViewState.updateCameraFrames(cameraFrames);
		const transitionCameraId = destinationCameraId ?? focusedCameraId;
		const dataUrl =
			(transitionCameraId ? cameraFrames[transitionCameraId] : null) ??
			(transitionCameraId ? peekViewState.cameraFrame(transitionCameraId) : null) ??
			captureTransitionFrame(destinationCameraId);
		const currentTransition = peekViewState.transition;
		if (currentTransition) peekViewState.finishTransition(currentTransition);
		if (!dataUrl) return;
		peekViewState.beginTransition({
			dataUrl,
			destination: destinationPath === viewerPath ? 'viewer' : 'dashboard',
			cameraId: destinationCameraId
		});
		await preloadTransitionFrame(dataUrl);
	});
	$effect(() => {
		if (loading || !livePlansReady) return;
		void livePeer.configure(livePlans).catch((error) => {
			console.error('Unable to configure shared live view', error);
		});
	});

	$effect(() => {
		void focusedCameraId;
		void focusQuality;
		void focusFallbackVariant;
		scheduleLivePlanReconcile();
	});

	$effect(() => {
		const track = focusedTrack;
		const selection = focusedVariantSelection;
		if (
			focusedCameraId === null ||
			track?.status !== 'unavailable' ||
			focusFallbackAttempted ||
			!selection
		) {
			return;
		}
		const fallback = selection.fallbackStreams.find((candidate) => candidate !== focusedVariant);
		focusFallbackAttempted = true;
		if (fallback) {
			focusFallbackVariant = fallback;
			focusRuntimeNotice = `${focusStreamLabel(focusedVariant)} live playback was unavailable. Showing ${focusStreamLabel(fallback)} instead.`;
			return;
		}
		focusRuntimeNotice = `${focusStreamLabel(focusedVariant)} live playback is unavailable and no compatible fallback was reported.`;
	});

	$effect(() => {
		const requestedExists = cameras.some((camera) => camera.id === requestedCameraId);
		if (!cameraViewActive) {
			if (legacyRootCameraId && requestedCameraId === legacyRootCameraId && requestedExists) {
				void goto(viewerHref(requestedCameraId), { replaceState: true });
			}
			return;
		}
		if (!viewerSelectionReady) return;
		const rememberedExists = cameras.some((camera) => camera.id === lastViewerCameraId);
		const cameraId = requestedExists
			? requestedCameraId
			: rememberedExists
				? lastViewerCameraId
				: cameras[0]?.id;
		if (!cameraId) return;
		if (focusedCameraId !== cameraId) activateFocus(cameraId);
		if (requestedCameraId !== cameraId) {
			void goto(viewerHref(cameraId), { replaceState: true, noScroll: true, keepFocus: true });
		}
	});

	$effect(() => {
		if (cameraViewActive || focusedCameraId === null || focusReturnPending) return;
		closeFocus();
	});

	onMount(() => {
		playbackPreferences = loadPlaybackPreferences(window.localStorage);
		lastViewerCameraId = window.localStorage.getItem(lastViewerCameraKey)?.trim() ?? '';
		viewerSelectionReady = true;
		const decoderBudget = webDecoderBudget(navigator.hardwareConcurrency);
		decoderCapacity = decoderBudget;
		gridScheduler.setCapacity({
			subscriptionSlots: decoderBudget,
			decoderSlots: decoderBudget
		});
		emitTimelinePerformanceEvent('DecoderCapacity', {
			decoderSlots: decoderBudget,
			subscriptionSlots: decoderBudget
		});
		const onVisibility = () => {
			screenActive = document.visibilityState === 'visible';
			reconcileLivePlans();
		};
		document.addEventListener('visibilitychange', onVisibility);
		void loadDashboard();
		const healthTimer = window.setInterval(() => {
			if (document.visibilityState === 'visible') void refreshHealth();
		}, healthRefreshIntervalMs);
		return () => {
			componentActive = false;
			document.removeEventListener('visibilitychange', onVisibility);
			window.clearInterval(healthTimer);
			if (schedulerTimer) clearTimeout(schedulerTimer);
			if (wallRevealTimer) clearTimeout(wallRevealTimer);
			clearBackgroundCadenceTimers();
		};
	});

	function previewStream(camera: CameraListItem): 'main' | 'sub' {
		return (
			camera.profiles.find((profile) => profile.stream === 'sub' && profile.encoding === 'h264')
				?.stream ??
			camera.profiles.find((profile) => profile.encoding === 'h264')?.stream ??
			camera.profiles.at(-1)?.stream ??
			'main'
		);
	}

	async function refreshHealth(): Promise<void> {
		if (healthRefreshInFlight) return;
		healthRefreshInFlight = true;
		const generation = peekViewState.generation;
		try {
			const health = await controlClient.getHealth();
			if (!peekViewState.updateHealth(generation, health)) return;
			publishShellHealth(health);
		} catch {
			// Retain the last authoritative snapshot until a later refresh succeeds.
		} finally {
			healthRefreshInFlight = false;
		}
	}

	async function loadDashboard(): Promise<void> {
		const coldStart = !peekViewState.loaded;
		const refresh = peekViewState.refresh(controlClient);
		if (!coldStart) {
			if (!peekViewState.wallRevealed || transition?.destination === 'dashboard') {
				armWallReveal();
			}
			await tick();
			if (!componentActive) return;
			revealCachedWall();
			reconcileLivePlans();
		}
		await refresh;
		if (!componentActive) return;
		publishShellHealth(peekViewState.serverHealth);
		if (coldStart) armWallReveal();
		await tick();
		if (!componentActive) return;
		reconcileLivePlans();
	}

	function scheduleLivePlanReconcile(): void {
		if (livePlanReconcileScheduled) return;
		livePlanReconcileScheduled = true;
		queueMicrotask(() => {
			livePlanReconcileScheduled = false;
			if (componentActive) reconcileLivePlans();
		});
	}

	function armWallReveal(): void {
		wallFrameCameraIds = new Set();
		wallRevealState = 'staging';
		const layoutCameraIds = activeLayout?.items.map((item) => item.cameraId);
		const targetCameras = layoutCameraIds
			? layoutCameraIds
					.map((cameraId) => cameras.find((camera) => camera.id === cameraId))
					.filter((camera): camera is CameraListItem => camera !== undefined)
			: cameras;
		wallTargetCameraIds = targetCameras
			.filter(
				(camera) =>
					camera.profiles.length > 0 &&
					presentPeekCamera(camera, cameraHealthById.get(camera.id) ?? null).state !== 'offline'
			)
			.slice(0, decoderCapacity)
			.map((camera) => camera.id);
		if (wallRevealTimer) clearTimeout(wallRevealTimer);
		if (wallTargetCameraIds.length === 0) {
			revealWall('frames');
			return;
		}
		wallRevealTimer = setTimeout(() => revealWall('timeout'), wallRevealTimeoutMs);
	}

	function handleWallFrameActivity(cameraId: string, active: boolean): void {
		if (!active || !wallTargetCameraIds.includes(cameraId)) {
			return;
		}
		const ready = new Set(wallFrameCameraIds);
		ready.add(cameraId);
		wallFrameCameraIds = ready;
		if (!wallTargetCameraIds.every((target) => ready.has(target))) return;
		if (wallRevealState === 'staging') {
			revealWall('frames');
			return;
		}
		if (wallRevealState !== 'frames') wallRevealState = 'frames';
		const currentTransition = peekViewState.transition;
		if (currentTransition?.destination === 'dashboard') {
			peekViewState.finishTransition(currentTransition);
		}
	}

	function revealCachedWall(): void {
		const currentTransition = peekViewState.transition;
		if (
			currentTransition?.destination === 'dashboard' &&
			wallTargetCameraIds.length > 0 &&
			wallTargetCameraIds.every((cameraId) => peekViewState.cameraFrame(cameraId) !== null)
		) {
			revealWall('cached');
		}
	}

	function revealWall(reason: 'cached' | 'frames' | 'timeout'): void {
		if (wallRevealState !== 'staging') return;
		wallRevealState = reason;
		peekViewState.markWallRevealed();
		if (wallRevealTimer) clearTimeout(wallRevealTimer);
		wallRevealTimer = null;
		const currentTransition = peekViewState.transition;
		if (reason !== 'timeout' && currentTransition?.destination === 'dashboard') {
			peekViewState.finishTransition(currentTransition);
		}
		if (focusReturnPending) finishFocusReturn();
	}

	function handleFocusFramePresented(frame: {
		width: number;
		height: number;
		stream: 'main' | 'sub';
		status: 'queued' | 'connecting' | 'live' | 'unavailable';
	}): void {
		const camera = focusedCamera;
		const track = focusedTrack;
		if (!camera || !track) return;
		const expectedStream = focusPreviewPresented ? focusedVariant : previewStream(camera);
		if (
			track.requestedVariantId !== expectedStream ||
			frame.status !== 'live' ||
			track.pendingStream !== null ||
			track.activeStream !== expectedStream ||
			frame.stream !== expectedStream
		) {
			return;
		}
		const expectedResolution = camera.profiles.find(
			(profile) => profile.stream === expectedStream
		)?.resolution;
		if (!videoResolutionMatches(expectedResolution, frame.width, frame.height)) return;
		const video = document.querySelector<HTMLVideoElement>('[data-peek-focus-stage] video');
		if (!video || !videoFrameIsVisible(video)) return;
		if (!focusPreviewPresented) {
			focusPreviewPresented = true;
			scheduleLivePlanReconcile();
		}
		const currentTransition = peekViewState.transition;
		if (currentTransition?.destination === 'viewer') {
			peekViewState.finishTransition(currentTransition);
		}
	}

	function videoFrameIsVisible(video: HTMLVideoElement): boolean {
		if (video.videoWidth <= 0 || video.videoHeight <= 0 || video.readyState < 2) return false;
		const canvas = document.createElement('canvas');
		canvas.width = 16;
		canvas.height = 9;
		const context = canvas.getContext('2d', { willReadFrequently: true });
		if (!context) return false;
		try {
			context.drawImage(video, 0, 0, canvas.width, canvas.height);
			const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
			let total = 0;
			for (let index = 0; index < pixels.length; index += 4) {
				total += pixels[index] + pixels[index + 1] + pixels[index + 2];
			}
			return total / (canvas.width * canvas.height * 3) > 8;
		} catch {
			return false;
		}
	}

	function captureTransitionFrame(destinationCameraId: string | null): string | null {
		const focusedVideo = document.querySelector<HTMLVideoElement>('[data-peek-focus-stage] video');
		const destinationVideo = destinationCameraId
			? document.querySelector<HTMLVideoElement>(
					`[data-peek-camera="${CSS.escape(destinationCameraId)}"] video`
				)
			: null;
		const video =
			focusedVideo ??
			destinationVideo ??
			document.querySelector<HTMLVideoElement>('[data-peek-camera] video');
		if (!video || video.videoWidth <= 0 || video.videoHeight <= 0 || video.readyState < 2) {
			return null;
		}
		return captureVideoFrame(video, transitionFrameMaxDimension, 0.82);
	}

	function captureCameraFrames(): Record<string, string> {
		const candidates = new Map<string, HTMLVideoElement>();
		for (const view of document.querySelectorAll<HTMLElement>('[data-camera-id]')) {
			const cameraId = view.dataset.cameraId;
			const video = view.querySelector<HTMLVideoElement>('video');
			if (!cameraId || !video || video.videoWidth <= 0 || video.videoHeight <= 0) continue;
			const previous = candidates.get(cameraId);
			if (
				previous &&
				previous.videoWidth * previous.videoHeight >= video.videoWidth * video.videoHeight
			) {
				continue;
			}
			candidates.set(cameraId, video);
		}
		return Object.fromEntries(
			[...candidates].flatMap(([cameraId, video]) => {
				const frame = captureVideoFrame(video, cameraFrameMaxDimension, 0.76);
				return frame ? [[cameraId, frame]] : [];
			})
		);
	}

	function captureCameraFrame(cameraId: string): string | null {
		const videos = [
			...document.querySelectorAll<HTMLVideoElement>(
				`[data-camera-id="${CSS.escape(cameraId)}"] video`
			)
		].filter((video) => video.videoWidth > 0 && video.videoHeight > 0);
		const video = videos.toSorted(
			(left, right) => right.videoWidth * right.videoHeight - left.videoWidth * left.videoHeight
		)[0];
		return video ? captureVideoFrame(video, cameraFrameMaxDimension, 0.76) : null;
	}

	async function preloadTransitionFrame(dataUrl: string): Promise<void> {
		const image = new Image();
		image.src = dataUrl;
		if (image.complete && image.naturalWidth > 0) return;
		await image.decode().catch(() => undefined);
	}

	function captureVideoFrame(
		video: HTMLVideoElement,
		maximumDimension: number,
		quality: number
	): string | null {
		if (video.videoWidth <= 0 || video.videoHeight <= 0 || video.readyState < 2) return null;
		const scale = Math.min(1, maximumDimension / Math.max(video.videoWidth, video.videoHeight));
		const canvas = document.createElement('canvas');
		canvas.width = Math.max(1, Math.round(video.videoWidth * scale));
		canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
		const context = canvas.getContext('2d');
		if (!context) return null;
		try {
			context.drawImage(video, 0, 0, canvas.width, canvas.height);
			return canvas.toDataURL('image/jpeg', quality);
		} catch {
			return null;
		}
	}

	function handleTileVisibility(visibility: GridTileVisibility): void {
		tileVisibility = { ...tileVisibility, [visibility.cameraId]: visibility };
		reconcileLivePlans();
	}

	function startBackgroundWarmWindow(): void {
		clearBackgroundCadenceTimers();
		backgroundStreamRate = 'full';
		backgroundPulseCameraIds = new Set();
		backgroundWarmTimer = setTimeout(() => {
			backgroundWarmTimer = null;
			backgroundStreamRate = '1fps';
			pulseBackgroundStreams();
			backgroundPulseTimer = setInterval(pulseBackgroundStreams, backgroundPulseIntervalMs);
		}, backgroundWarmDurationMs);
		scheduleLivePlanReconcile();
	}

	function stopBackgroundCadence(): void {
		clearBackgroundCadenceTimers();
		backgroundStreamRate = 'full';
		backgroundPulseCameraIds = new Set();
	}

	function clearBackgroundCadenceTimers(): void {
		if (backgroundWarmTimer) clearTimeout(backgroundWarmTimer);
		if (backgroundPulseTimer) clearInterval(backgroundPulseTimer);
		if (backgroundPulseEndTimer) clearTimeout(backgroundPulseEndTimer);
		backgroundWarmTimer = null;
		backgroundPulseTimer = null;
		backgroundPulseEndTimer = null;
	}

	function pulseBackgroundStreams(): void {
		if (
			focusedCameraId === null ||
			backgroundStreamRate !== '1fps' ||
			backgroundPulseCameraIds.size > 0
		) {
			return;
		}
		backgroundPulseCameraIds = new Set(
			cameras
				.filter((camera) => camera.id !== focusedCameraId && camera.profiles.length > 0)
				.map((camera) => camera.id)
		);
		if (backgroundPulseCameraIds.size === 0) return;
		backgroundPulseEndTimer = setTimeout(finishBackgroundPulse, backgroundPulseMaximumDurationMs);
		scheduleLivePlanReconcile();
	}

	function finishBackgroundPulse(): void {
		backgroundPulseEndTimer = null;
		if (backgroundPulseCameraIds.size === 0) return;
		backgroundPulseCameraIds = new Set();
		scheduleLivePlanReconcile();
	}

	function handleBackgroundFramePresented(cameraId: string): void {
		if (backgroundStreamRate !== '1fps' || !backgroundPulseCameraIds.has(cameraId)) return;
		const remaining = new Set(backgroundPulseCameraIds);
		remaining.delete(cameraId);
		backgroundPulseCameraIds = remaining;
		if (remaining.size === 0 && backgroundPulseEndTimer) {
			clearTimeout(backgroundPulseEndTimer);
			backgroundPulseEndTimer = null;
		}
		scheduleLivePlanReconcile();
	}

	function reconcileLivePlans(): void {
		if (schedulerTimer) {
			clearTimeout(schedulerTimer);
			schedulerTimer = null;
		}
		const availableCameras = cameras.filter(
			(camera) =>
				camera.profiles.length > 0 &&
				presentPeekCamera(camera, cameraHealthById.get(camera.id) ?? null).state !== 'offline'
		);
		const demands: GridTileDemand[] = availableCameras.map((camera) => {
			const visibility = tileVisibility[camera.id];
			const focused = !focusReturnPending && focusedCameraId === camera.id;
			const staging = wallRevealState === 'staging' && wallTargetCameraIds.includes(camera.id);
			return {
				cameraId: camera.id,
				visibleFraction: focused || staging ? 1 : (visibility?.visibleFraction ?? 0),
				distanceFromViewportPx: focused
					? 0
					: staging
						? 0
						: (visibility?.distanceFromViewportPx ?? Number.POSITIVE_INFINITY),
				viewportExtentPx: visibility?.viewportExtentPx ?? Math.max(1, window.innerHeight),
				focused,
				fullscreen: false,
				selectedForAudio: false,
				screenActive,
				mode: 'live'
			};
		});
		const nowMs = performance.now();
		const previouslyActive = new Set(
			livePlans.filter((plan) => plan.active).map((plan) => plan.cameraId)
		);
		const schedule = gridScheduler.reconcile(demands, nowMs);
		const grants = new Map(schedule.grants.map((grant) => [grant.cameraId, grant]));
		livePlans = availableCameras.map((camera) => {
			const focused = !focusReturnPending && focusedCameraId === camera.id;
			const focusVariant = focusPreviewPresented ? focusedVariant : previewStream(camera);
			const backgroundActive =
				focusedCameraId !== null &&
				(backgroundStreamRate === 'full' || backgroundPulseCameraIds.has(camera.id));
			return {
				cameraId: camera.id,
				quality:
					focused && focusPreviewPresented
						? effectiveFocusQuality
						: (grants.get(camera.id)?.quality ?? ('low' as const)),
				active:
					screenActive &&
					(focusedCameraId === null ? grants.has(camera.id) : focused || backgroundActive),
				variantId: focused ? focusVariant : previewStream(camera)
			};
		});
		livePlansReady = true;
		for (const cameraId of grants.keys()) {
			if (!previouslyActive.has(cameraId)) {
				emitTimelinePerformanceEvent('GridTileAdmitted', { sourceId: cameraId });
			}
		}
		for (const cameraId of previouslyActive) {
			if (!grants.has(cameraId)) {
				emitTimelinePerformanceEvent('GridTileEvicted', { sourceId: cameraId });
			}
		}
		if (schedule.nextReconcileAtMs !== null) {
			schedulerTimer = window.setTimeout(
				reconcileLivePlans,
				Math.max(0, schedule.nextReconcileAtMs - nowMs)
			);
		}
	}

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function cameraHealth(cameraId: string): CameraHealth | null {
		return cameraHealthById.get(cameraId) ?? null;
	}

	function viewerHref(cameraId?: string): string {
		return cameraId
			? `${resolve('/viewer')}?camera=${encodeURIComponent(cameraId)}`
			: resolve('/viewer');
	}

	function rememberViewerCamera(cameraId: string): void {
		lastViewerCameraId = cameraId;
		window.localStorage.setItem(lastViewerCameraKey, cameraId);
	}

	function activateFocus(cameraId: string): void {
		const enteringFocus = focusedCameraId === null;
		rememberViewerCamera(cameraId);
		focusReturnPending = false;
		if (wallRevealTimer) clearTimeout(wallRevealTimer);
		wallRevealTimer = null;
		focusedCameraId = cameraId;
		focusQuality = focusedLivePreference(playbackPreferences, cameraId);
		focusFallbackVariant = null;
		focusFallbackAttempted = false;
		focusPreviewPresented = false;
		focusRuntimeNotice = null;
		if (enteringFocus) startBackgroundWarmWindow();
		scheduleLivePlanReconcile();
	}

	function openFocus(cameraId: string): void {
		if (!cameraViewActive) {
			rememberViewerCamera(cameraId);
			void goto(viewerHref(cameraId), { noScroll: true, keepFocus: true });
			return;
		}
		activateFocus(cameraId);
		if (requestedCameraId !== cameraId) {
			void goto(viewerHref(cameraId), { noScroll: true, keepFocus: true });
		}
	}

	function setFocusQuality(quality: FocusedLivePreference) {
		if (focusQuality === quality) return;
		focusQuality = quality;
		focusFallbackVariant = null;
		focusFallbackAttempted = false;
		focusRuntimeNotice = null;
		if (focusedCameraId) {
			playbackPreferences = withFocusedLivePreference(
				playbackPreferences,
				focusedCameraId,
				quality
			);
			savePlaybackPreferences(window.localStorage, playbackPreferences);
		}
	}

	function closeFocus() {
		if (focusedCameraId === null || focusReturnPending) return;
		const returnToDashboard = page.url.pathname === resolve('/viewer');
		if (returnToDashboard) {
			void goto(resolve('/'));
			return;
		}
		if (wallRevealState !== 'staging') {
			finishFocusReturn();
			return;
		}
		focusReturnPending = true;
		armWallReveal();
		scheduleLivePlanReconcile();
	}

	function finishFocusReturn() {
		const previousCameraId = focusedCameraId;
		stopBackgroundCadence();
		if (wallRevealTimer) clearTimeout(wallRevealTimer);
		wallRevealTimer = null;
		focusReturnPending = false;
		focusedCameraId = null;
		focusFallbackVariant = null;
		focusPreviewPresented = false;
		focusRuntimeNotice = null;
		scheduleLivePlanReconcile();
		if (previousCameraId !== null) {
			void tick().then(() => {
				if (!componentActive) return;
				document
					.querySelector<HTMLElement>(`[data-peek-focus="${CSS.escape(previousCameraId)}"]`)
					?.focus();
			});
		}
	}

	function cameraHref(cameraId: string): string {
		return `${resolve('/camera')}?camera=${encodeURIComponent(cameraId)}`;
	}

	function historyHref(cameraId: string): string {
		return `${resolve('/keep')}?camera=${encodeURIComponent(cameraId)}`;
	}

	function focusStreamLabel(stream: 'main' | 'sub'): string {
		return stream === 'main' ? 'Main' : 'Sub';
	}

	async function selectDashboard(dashboardId: string): Promise<void> {
		if (layoutRegistry === null) return;
		if (dashboardId === layoutRegistry.activeLayoutId) return;
		layoutSaving = true;
		const generation = peekViewState.generation;
		peekViewState.updateLayoutError(generation, null);
		try {
			const savedRegistry = await controlClient.savePeekLayoutRegistry(
				selectPeekLayout(layoutRegistry, dashboardId)
			);
			if (!peekViewState.updateLayoutRegistry(generation, savedRegistry)) return;
			armWallReveal();
			await tick();
			reconcileLivePlans();
		} catch (cause) {
			peekViewState.updateLayoutError(
				generation,
				cause instanceof Error ? cause.message : 'Dashboard selection was not saved.'
			);
		} finally {
			layoutSaving = false;
		}
	}

	function moveGridFocus(event: KeyboardEvent): void {
		const target = event.target;
		if (!(target instanceof HTMLElement)) return;
		const current = target.closest<HTMLElement>('[data-peek-camera]');
		if (!current) return;
		const direction = event.key;
		if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(direction)) return;
		const currentBounds = current.getBoundingClientRect();
		const currentX = currentBounds.left + currentBounds.width / 2;
		const currentY = currentBounds.top + currentBounds.height / 2;
		const candidates = [...document.querySelectorAll<HTMLElement>('[data-peek-focus]')]
			.filter((button) => button !== target)
			.map((button) => {
				const tile = button.closest<HTMLElement>('[data-peek-camera]');
				const bounds = tile?.getBoundingClientRect();
				if (!bounds) return null;
				const deltaX = bounds.left + bounds.width / 2 - currentX;
				const deltaY = bounds.top + bounds.height / 2 - currentY;
				const inDirection =
					(direction === 'ArrowLeft' && deltaX < 0) ||
					(direction === 'ArrowRight' && deltaX > 0) ||
					(direction === 'ArrowUp' && deltaY < 0) ||
					(direction === 'ArrowDown' && deltaY > 0);
				if (!inDirection) return null;
				const primary =
					direction === 'ArrowLeft' || direction === 'ArrowRight'
						? Math.abs(deltaX)
						: Math.abs(deltaY);
				const secondary =
					direction === 'ArrowLeft' || direction === 'ArrowRight'
						? Math.abs(deltaY)
						: Math.abs(deltaX);
				return { button, score: primary + secondary * 2 };
			})
			.filter(
				(candidate): candidate is { button: HTMLElement; score: number } => candidate !== null
			)
			.toSorted((left, right) => left.score - right.score);
		const next = candidates[0]?.button;
		if (!next) return;
		event.preventDefault();
		next.focus();
	}

	function cycleFocusedCamera(direction: -1 | 1): void {
		if (focusedCameraId === null || cameras.length < 2) return;
		const currentIndex = cameras.findIndex((camera) => camera.id === focusedCameraId);
		if (currentIndex < 0) return;
		const nextIndex = (currentIndex + direction + cameras.length) % cameras.length;
		const nextCamera = cameras[nextIndex];
		if (nextCamera) openFocus(nextCamera.id);
	}

	function handleKeydown(event: KeyboardEvent) {
		if (isKeyboardTypingTarget(event.target) || event.metaKey || event.ctrlKey || event.altKey)
			return;
		if (focusedCameraId !== null && (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
			event.preventDefault();
			event.stopImmediatePropagation();
			cycleFocusedCamera(event.key === 'ArrowLeft' ? -1 : 1);
			return;
		}
		if (focusedCameraId !== null && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
			event.preventDefault();
			event.stopImmediatePropagation();
			closeFocus();
			return;
		}
		if (event.key === 'Escape' && focusedCameraId !== null) {
			event.preventDefault();
			closeFocus();
			return;
		}
		if (event.key.toLowerCase() === 'f' && focusedCameraId !== null) {
			event.preventDefault();
			event.stopImmediatePropagation();
			closeFocus();
			return;
		}
		moveGridFocus(event);
		const target = event.target;
		if (!(target instanceof HTMLElement)) return;
		const cameraId = target.closest<HTMLElement>('[data-peek-camera]')?.dataset.peekCamera;
		if (!cameraId) return;
		if (event.key === 'Enter') {
			event.preventDefault();
			openFocus(cameraId);
			return;
		}
		if (event.key.toLowerCase() !== 'f') return;
		event.preventDefault();
		openFocus(cameraId);
	}
</script>

<svelte:head>
	<title>{cameraViewActive ? 'Viewer' : 'Dashboard'} - KeepPeek</title>
</svelte:head>

<svelte:window onkeydown={handleKeydown} />

<div data-peek-view class="peek-view absolute inset-0 flex min-h-0 min-w-0 flex-col">
	{#if !cameraViewActive}
		<h1 class="sr-only">Dashboard</h1>
	{/if}
	{#if !cameraViewActive && !loading && (focusedCamera === null || focusReturnPending)}
		<PeekDashboardSwitcher
			layouts={layoutRegistry?.layouts ?? []}
			{activeLayout}
			busy={layoutSaving}
			onselect={selectDashboard}
		/>
		{#if layoutError}
			<p
				class="absolute top-14 left-1/2 z-30 max-w-sm -translate-x-1/2 rounded-sm border border-destructive/40 bg-background/95 px-2.5 py-1.5 text-xs text-destructive shadow-lg"
				role="alert"
			>
				{layoutError}
			</p>
		{/if}
	{/if}
	<div data-peek-view-content class="peek-view-content">
		{#if loading}
			<div
				data-peek-layout-loading
				class="size-full"
				role="status"
				aria-label="Loading live view"
			></div>
		{:else if error}
			<div
				class="rounded-md border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
				role="alert"
			>
				{error}
			</div>
		{:else if cameras.length === 0}
			<div
				class="grid size-full min-h-64 place-items-center border-y text-sm text-muted-foreground"
			>
				No cameras configured.
			</div>
		{:else}
			<div class="grid size-full min-h-0 md:flex-1">
				{#if focusedCamera}
					<section
						data-background-stream-rate={backgroundStreamRate}
						data-focused-live-preference={focusQuality}
						data-focused-live-selected-variant={focusedVariant}
						data-focused-live-fallback-variant={focusFallbackVariant ?? undefined}
						data-peek-focus-return={focusReturnPending ? 'waiting' : undefined}
						class="focus-surface relative z-10 col-start-1 row-start-1 h-full min-h-0 overflow-hidden bg-background"
						aria-label={`${cameraLabel(focusedCamera)} focus`}
						aria-busy={focusReturnPending}
					>
						<div
							data-peek-focus-stage
							data-peek-focus-history
							class="focus-stage group absolute inset-0 overflow-hidden bg-black"
						>
							{#key focusedCamera.id}
								<LiveVideo
									cameraId={focusedCamera.id}
									stream={focusPreviewPresented ? focusedVariant : previewStream(focusedCamera)}
									quality={effectiveFocusQuality}
									fallbackFrameUrl={peekViewState.cameraFrame(focusedCamera.id)}
									diagnosticsLabel={cameraLabel(focusedCamera)}
									diagnosticsStatusClass={focusedDiagnosticsStatusClass}
									diagnosticsRecording={focusedRecordingDiagnostics}
									cameraHref={cameraHref(focusedCamera.id)}
									onframepresented={handleFocusFramePresented}
									onvisibilitychange={handleTileVisibility}
									class="size-full min-h-0 overflow-hidden"
								/>
							{/key}
						</div>

						<div data-peek-focus-options class="pointer-events-none absolute inset-0 z-30">
							<div class="focus-controls pointer-events-auto">
								<div
									class="focus-mode-options rounded-sm bg-video/70 p-0.5 text-white shadow-md ring-1 ring-white/10 backdrop-blur-md"
								>
									<span
										class="flex h-7 items-center justify-center gap-1.5 rounded-xs bg-white px-2 text-[11px] font-medium text-black"
										aria-current="page"
									>
										<RadioIcon class="size-3.5" />
										Live
									</span>
									<!-- eslint-disable svelte/no-navigation-without-resolve -->
									<a
										href={historyHref(focusedCamera.id)}
										class="flex h-7 items-center justify-center gap-1.5 rounded-xs px-2 text-[11px] font-medium text-white/65 hover:text-white focus-visible:ring-2 focus-visible:ring-white focus-visible:outline-none"
									>
										<HistoryIcon class="size-3.5" />
										History
									</a>
									<!-- eslint-enable svelte/no-navigation-without-resolve -->
								</div>
								<div
									class="focus-quality-options rounded-sm bg-video/70 p-0.5 text-white shadow-md ring-1 ring-white/10 backdrop-blur-md"
									role="group"
									aria-label="Live quality ceiling"
								>
									{#each qualityOptions as option (option.value)}
										<button
											type="button"
											class="h-7 rounded px-2 text-[11px] font-medium {focusQuality === option.value
												? 'bg-white text-black'
												: 'text-white/65 hover:text-white'}"
											aria-pressed={focusQuality === option.value}
											onclick={() => setFocusQuality(option.value)}
										>
											{option.label}
										</button>
									{/each}
								</div>
								{#if backgroundStreamRate === '1fps'}
									<button
										type="button"
										class="flex h-8 shrink-0 items-center gap-1.5 rounded-sm bg-white px-2.5 text-[11px] font-medium text-black shadow-md focus-visible:ring-2 focus-visible:ring-white focus-visible:outline-none"
										aria-label="Increase background FPS"
										title="Increase background FPS for five minutes"
										onclick={startBackgroundWarmWindow}
									>
										<GaugeIcon class="size-3.5" />
										Increase FPS
									</button>
								{/if}
								{#if focusNotice}
									<span
										class="focus-layout-status rounded-sm bg-video/75 px-2 py-1 text-xs text-amber-200 shadow-md backdrop-blur-md"
										role="status"
									>
										{focusNotice}
									</span>
								{/if}
							</div>

							<aside
								class="focus-camera-options pointer-events-auto rounded-md bg-video/65 p-1.5 shadow-lg ring-1 ring-white/10 backdrop-blur-md"
								aria-label="Camera filmstrip"
							>
								{#each cameras as camera (camera.id)}
									<article
										data-focus-camera-option={camera.id}
										class="focus-camera-option {camera.id === focusedCamera.id
											? 'ring-2 ring-primary'
											: 'ring-1 ring-white/15'}"
									>
										<LiveVideo
											cameraId={camera.id}
											stream="sub"
											showDiagnostics={false}
											onframepresented={(frame) => {
												if (frame.status === 'live' && frame.stream === 'sub') {
													handleBackgroundFramePresented(camera.id);
												}
											}}
											onvisibilitychange={handleTileVisibility}
											class="size-full overflow-hidden"
										/>
										<button
											type="button"
											class="absolute inset-0 z-10 focus-visible:ring-2 focus-visible:ring-white focus-visible:outline-none focus-visible:ring-inset"
											aria-label={`Focus ${cameraLabel(camera)}`}
											aria-pressed={camera.id === focusedCamera.id}
											onclick={() => openFocus(camera.id)}
										></button>
									</article>
								{/each}
							</aside>
						</div>
					</section>
				{/if}
				<div
					data-peek-wall
					data-peek-wall-state={wallRevealed ? 'ready' : 'staging'}
					data-peek-wall-reveal={wallRevealState === 'staging' ? undefined : wallRevealState}
					data-peek-wall-ready-count={wallFrameCameraIds.size}
					data-peek-wall-target-count={wallTargetCameraIds.length}
					class="peek-wall-frame relative col-start-1 row-start-1 {activeLayout
						? 'md:h-full md:min-h-0'
						: ''} {focusedCamera === null ? '' : 'pointer-events-none opacity-0'}"
					aria-busy={!wallRevealed}
					aria-hidden={focusedCamera !== null}
				>
					<div
						data-peek-wall-content
						data-peek-layout-id={activeLayout?.id}
						inert={!wallRevealed || focusedCamera !== null}
						class="layout-wall grid grid-cols-2 gap-2.5 transition-[opacity,transform] duration-500 ease-out motion-reduce:transform-none motion-reduce:transition-none md:grid-cols-3 {activeLayout
							? 'saved-layout'
							: '2xl:grid-cols-4'} {wallRevealed
							? 'translate-y-0 opacity-100'
							: 'pointer-events-none translate-y-5 opacity-0'}"
					>
						{#if activeLayout}
							{#each activeLayout.items as item, cameraIndex (item.cameraId)}
								{@const camera = cameras.find((candidate) => candidate.id === item.cameraId)}
								<div
									class="layout-tile min-h-0 min-w-0"
									style={`--layout-column:${item.column} / span ${item.columnSpan};--layout-row:${item.row} / span ${item.rowSpan}`}
								>
									{#if camera}
										<PeekCameraTile
											{camera}
											health={cameraHealth(camera.id)}
											stream={previewStream(camera)}
											fallbackFrameUrl={peekViewState.cameraFrame(camera.id)}
											mobileFeatured={cameraIndex === 0}
											onframepresented={handleBackgroundFramePresented}
											onframeactivitychange={handleWallFrameActivity}
											onvisibilitychange={handleTileVisibility}
											onfocus={openFocus}
										/>
									{:else}
										<article
											data-peek-missing-camera={item.cameraId}
											class="grid size-full min-h-28 place-items-center rounded-lg border border-dashed border-hairline-strong bg-surface p-4 text-center"
										>
											<div class="space-y-1">
												<CameraIcon class="mx-auto size-5 text-text-faint" />
												<p class="text-xs font-medium">Camera unavailable</p>
												<p class="font-mono text-2xs text-text-muted">{item.cameraId}</p>
											</div>
										</article>
									{/if}
								</div>
							{/each}
						{:else}
							{#each cameras as camera, cameraIndex (camera.id)}
								<PeekCameraTile
									{camera}
									health={cameraHealth(camera.id)}
									stream={previewStream(camera)}
									fallbackFrameUrl={peekViewState.cameraFrame(camera.id)}
									mobileFeatured={cameraIndex === 0}
									onframepresented={handleBackgroundFramePresented}
									onframeactivitychange={handleWallFrameActivity}
									onvisibilitychange={handleTileVisibility}
									onfocus={openFocus}
								/>
							{/each}
						{/if}
					</div>
					<div
						data-peek-wall-placeholder
						class="layout-wall pointer-events-none absolute inset-0 grid grid-cols-2 gap-2.5 transition-[opacity,transform] duration-300 ease-out motion-reduce:transform-none motion-reduce:transition-none md:grid-cols-3 {activeLayout
							? 'saved-layout'
							: '2xl:grid-cols-4'} {wallRevealed
							? '-translate-y-3 opacity-0'
							: 'translate-y-0 opacity-100'}"
						aria-hidden="true"
					>
						{#if activeLayout}
							{#each activeLayout.items as item (item.cameraId)}
								<div
									class="layout-tile min-h-0 min-w-0"
									style={`--layout-column:${item.column} / span ${item.columnSpan};--layout-row:${item.row} / span ${item.rowSpan}`}
								>
									<Skeleton class="size-full min-h-28 rounded-lg" />
								</div>
							{/each}
						{:else}
							{#each cameras as camera, cameraIndex (camera.id)}
								<Skeleton
									class="w-full rounded-lg {cameraIndex === 0
										? 'col-span-2 aspect-video md:col-span-1'
										: 'aspect-[174/110] md:aspect-video'}"
								/>
							{/each}
						{/if}
					</div>
				</div>
			</div>
		{/if}
	</div>
	{#if transition}
		<div
			data-peek-transition={transition.destination}
			class="pointer-events-auto absolute inset-0 z-40 grid place-items-center overflow-hidden bg-black"
			aria-busy="true"
		>
			<img
				data-peek-transition-frame
				src={transition.dataUrl}
				alt=""
				class="size-full object-contain"
			/>
			<div
				class="absolute top-3 left-1/2 flex -translate-x-1/2 items-center gap-2 rounded-sm border border-amber-400/40 bg-black/75 px-2.5 py-1.5 text-xs font-medium text-amber-200 shadow-md backdrop-blur-md"
				role="status"
				aria-live="polite"
			>
				<span class="size-1.5 rounded-full bg-amber-400"></span>
				Restoring dashboard
			</div>
		</div>
	{/if}
</div>

<style>
	.peek-view-content {
		position: relative;
		display: flex;
		min-height: 0;
		min-width: 0;
		flex: 1;
		flex-direction: column;
		overflow: hidden;
	}

	.focus-mode-options,
	.focus-quality-options {
		display: flex;
	}

	.focus-controls {
		position: absolute;
		top: 0.75rem;
		left: 50%;
		display: flex;
		max-width: calc(100% - 24rem);
		transform: translateX(-50%);
		align-items: center;
		gap: 0.5rem;
		overflow-x: auto;
		overflow-y: hidden;
	}

	.focus-camera-options {
		position: absolute;
		bottom: 0.75rem;
		left: 50%;
		display: flex;
		width: max-content;
		max-width: calc(100% - 1.5rem);
		transform: translateX(-50%);
		gap: 0.375rem;
		overflow-x: auto;
		overflow-y: hidden;
	}

	.focus-camera-option {
		position: relative;
		aspect-ratio: 16 / 9;
		width: 8rem;
		flex: none;
		overflow: hidden;
		border-radius: 0.25rem;
	}

	.focus-stage {
		display: grid;
		place-items: center;
	}

	.focus-layout-status {
		max-width: 12rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	@media (max-width: 47.999rem) {
		.focus-controls {
			top: 3.25rem;
			right: 0.75rem;
			left: 0.75rem;
			max-width: none;
			transform: none;
		}

		.focus-camera-option {
			width: 7rem;
		}
	}

	@media (min-width: 48rem) {
		.peek-wall-frame:has(.saved-layout) {
			container-type: size;
		}

		.layout-wall.saved-layout {
			width: min(100cqw, calc(100cqh * 16 / 9));
			height: min(100cqh, calc(100cqw * 9 / 16));
			margin-inline: auto;
			grid-template-columns: repeat(12, minmax(0, 1fr));
			grid-template-rows: repeat(12, minmax(0, 1fr));
		}

		.layout-wall.saved-layout .layout-tile {
			grid-column: var(--layout-column);
			grid-row: var(--layout-row);
		}

		.layout-wall.saved-layout .layout-tile :global([data-peek-camera]) {
			width: 100%;
			height: 100%;
			aspect-ratio: auto;
		}
	}
</style>
