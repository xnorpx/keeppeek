<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { onMount, tick } from 'svelte';
	import type { CameraHealth, CameraListItem, LiveQuality, ServerHealthResponse } from '$lib/types';
	import { useControlClient } from '$lib/control-context';
	import { useLivePeer } from '$lib/stream-peer-context';
	import type { LivePeerPlan } from '$lib/stream-peer.svelte';
	import type { GridTileVisibility } from '$lib/grid-visibility';
	import { emitTimelinePerformanceEvent } from '$lib/timeline-observability';
	import {
		GridStreamScheduler,
		type GridTileDemand,
		webDecoderBudget
	} from '$lib/grid-stream-scheduler';
	import LiveVideo from '$lib/components/LiveVideo.svelte';
	import PeekCameraTile from '$lib/components/PeekCameraTile.svelte';
	import PeekLayoutEditor from '$lib/components/PeekLayoutEditor.svelte';
	import { presentPeekCamera } from '$lib/peek-camera';
	import { reconcileServerHealth } from '$lib/health-presentation';
	import { isKeyboardTypingTarget } from '$lib/keyboard-shortcuts';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import RadioIcon from '@lucide/svelte/icons/radio';

	const placeholders = [0, 1, 2, 3, 4, 5, 6, 7, 8] as const;
	const qualityOptions = ['auto', 'high', 'low'] as const;
	const wallRevealTimeoutMs = 5_000;
	const controlClient = useControlClient();
	const livePeer = useLivePeer();
	const gridScheduler = new GridStreamScheduler({ subscriptionSlots: 4, decoderSlots: 4 });

	let serverHealth = $state.raw<ServerHealthResponse | null>(null);
	let cameras = $state.raw<CameraListItem[]>([]);
	let error: string | null = $state(null);
	let loading = $state(true);
	let focusedCameraId: string | null = $state(null);
	let requestedCameraId = $derived(page.url.searchParams.get('camera')?.trim() ?? '');
	let isLayoutEditing = $derived(page.url.searchParams.get('mode') === 'layout-editor');
	let focusQuality = $state<LiveQuality>('auto');
	let livePlans = $state.raw<LivePeerPlan[]>([]);
	let tileVisibility = $state.raw<Record<string, GridTileVisibility>>({});
	let screenActive = true;
	let schedulerTimer: number | null = null;
	let decoderCapacity = 4;
	let wallFrameCameraIds = $state.raw<ReadonlySet<string>>(new Set());
	let wallTargetCameraIds = $state.raw<readonly string[]>([]);
	let wallRevealState = $state<'staging' | 'frames' | 'timeout'>('staging');
	let wallRevealTimer: ReturnType<typeof setTimeout> | null = null;
	let focusReturnPending = $state(false);
	let wallRevealed = $derived(wallRevealState !== 'staging');
	let focusedCamera = $derived(
		focusedCameraId === null
			? null
			: (cameras.find((camera) => camera.id === focusedCameraId) ?? null)
	);
	let focusedTrack = $derived(focusedCameraId === null ? null : livePeer.track(focusedCameraId));
	let pendingFocusStream = $derived(focusedTrack?.pendingStream ?? null);
	let focusQualitySwitching = $derived(pendingFocusStream !== null);
	let filmstripCameras = $derived(
		focusedCameraId === null ? [] : cameras.filter((camera) => camera.id !== focusedCameraId)
	);
	let cameraHealthById = $derived(
		new Map((serverHealth?.cameras ?? []).map((camera) => [camera.id, camera]))
	);
	let healthyCameraCount = $derived(
		(serverHealth?.cameras ?? []).filter((camera) => camera.state === 'online').length
	);
	let fleetStatus = $derived.by(() => {
		if (serverHealth?.status === 'healthy' && cameras.length === 0) {
			return {
				colorClass: 'bg-emerald-500',
				label: 'System online',
				showCameraCount: true
			};
		}
		if (serverHealth?.status === 'healthy') {
			return {
				colorClass: 'bg-emerald-500',
				label: `${healthyCameraCount} / ${cameras.length} cameras healthy`,
				showCameraCount: false
			};
		}

		if (healthyCameraCount > 0) {
			return {
				colorClass: 'bg-amber-500',
				label: `${healthyCameraCount} / ${cameras.length} cameras healthy`,
				showCameraCount: false
			};
		}

		return {
			colorClass: 'bg-destructive',
			label: 'System unavailable',
			showCameraCount: true
		};
	});
	let runtimeTelemetry = $derived.by(() => {
		if (
			serverHealth === null ||
			(serverHealth.system.memory.total_bytes === 0 &&
				serverHealth.system.process.cpu_capacity_percent === null &&
				serverHealth.system.process.resident_memory_bytes === null)
		) {
			return null;
		}
		return {
			hostCpu: formatPercent(serverHealth.system.system_cpu_percent),
			hostMemory: formatMemoryUsage(
				serverHealth.system.memory.used_bytes,
				serverHealth.system.memory.total_bytes
			),
			processCpu: formatPercent(serverHealth.system.process.cpu_capacity_percent),
			processMemory: formatBytes(serverHealth.system.process.resident_memory_bytes)
		};
	});
	$effect(() => {
		if (loading) return;
		void livePeer.configure(livePlans).catch((error) => {
			console.error('Unable to configure shared live view', error);
		});
	});

	$effect(() => {
		void isLayoutEditing;
		void focusedCameraId;
		void focusQuality;
		queueMicrotask(reconcileLivePlans);
	});

	$effect(() => {
		if (
			requestedCameraId &&
			focusedCameraId !== requestedCameraId &&
			cameras.some((camera) => camera.id === requestedCameraId)
		) {
			openFocus(requestedCameraId);
		}
	});

	onMount(() => {
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
		return () => {
			document.removeEventListener('visibilitychange', onVisibility);
			if (schedulerTimer) clearTimeout(schedulerTimer);
			if (wallRevealTimer) clearTimeout(wallRevealTimer);
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

	async function loadDashboard() {
		try {
			const [nextCameras, nextServerHealth] = await Promise.all([
				controlClient.getCameras(),
				controlClient.getHealth()
			]);
			cameras = nextCameras;
			serverHealth = reconcileServerHealth(nextServerHealth);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to load dashboard';
		} finally {
			loading = false;
			armWallReveal();
			await tick();
			reconcileLivePlans();
		}
	}

	function armWallReveal(): void {
		wallFrameCameraIds = new Set();
		wallRevealState = 'staging';
		wallTargetCameraIds = cameras
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
		if (!active || wallRevealState !== 'staging' || !wallTargetCameraIds.includes(cameraId)) {
			return;
		}
		const ready = new Set(wallFrameCameraIds);
		ready.add(cameraId);
		wallFrameCameraIds = ready;
		if (wallTargetCameraIds.every((target) => ready.has(target))) revealWall('frames');
	}

	function revealWall(reason: 'frames' | 'timeout'): void {
		if (wallRevealState !== 'staging') return;
		wallRevealState = reason;
		if (wallRevealTimer) clearTimeout(wallRevealTimer);
		wallRevealTimer = null;
		if (focusReturnPending) finishFocusReturn();
	}

	function handleTileVisibility(visibility: GridTileVisibility): void {
		tileVisibility = { ...tileVisibility, [visibility.cameraId]: visibility };
		reconcileLivePlans();
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
				screenActive: screenActive && !isLayoutEditing,
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
			return {
				cameraId: camera.id,
				quality: focused ? focusQuality : (grants.get(camera.id)?.quality ?? ('low' as const)),
				active: grants.has(camera.id),
				variantId: focused && focusQuality === 'auto' ? ('main' as const) : undefined
			};
		});
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

	function formatPercent(value: number | null): string {
		if (value === null) return '—';
		return `${value.toFixed(value >= 10 || value === 0 ? 0 : 1)}%`;
	}

	function formatBytes(bytes: number | null): string {
		if (bytes === null) return '—';
		if (bytes < 1_000) return `${bytes} B`;
		const units = ['kB', 'MB', 'GB', 'TB'];
		let value = bytes / 1_000;
		let unitIndex = 0;
		while (value >= 1_000 && unitIndex < units.length - 1) {
			value /= 1_000;
			unitIndex += 1;
		}
		return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unitIndex]}`;
	}

	function formatMemoryUsage(usedBytes: number, totalBytes: number): string {
		if (totalBytes <= 0) return '—';
		const units = ['B', 'kB', 'MB', 'GB', 'TB'];
		let divisor = 1;
		let unitIndex = 0;
		while (totalBytes / divisor >= 1_000 && unitIndex < units.length - 1) {
			divisor *= 1_000;
			unitIndex += 1;
		}
		const formatValue = (value: number) => value.toFixed(value >= 10 || value === 0 ? 0 : 1);
		return `${formatValue(usedBytes / divisor)}/${formatValue(totalBytes / divisor)} ${units[unitIndex]}`;
	}

	function openFocus(cameraId: string) {
		focusReturnPending = false;
		if (wallRevealTimer) clearTimeout(wallRevealTimer);
		wallRevealTimer = null;
		focusedCameraId = cameraId;
		focusQuality = 'auto';
		queueMicrotask(reconcileLivePlans);
	}

	function setFocusQuality(quality: LiveQuality) {
		if (focusQuality === quality) return;
		focusQuality = quality;
	}

	function closeFocus() {
		if (focusedCameraId === null || focusReturnPending) return;
		focusReturnPending = true;
		armWallReveal();
		queueMicrotask(reconcileLivePlans);
	}

	function finishFocusReturn() {
		const previousCameraId = focusedCameraId;
		focusReturnPending = false;
		focusedCameraId = null;
		focusQuality = 'auto';
		queueMicrotask(reconcileLivePlans);
		if (previousCameraId !== null) {
			void tick().then(() => {
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
		return `${resolve('/keep')}?camera=${encodeURIComponent(cameraId)}&stream=main`;
	}

	function openLayoutEditor(): void {
		void goto(`${resolve('/')}?mode=layout-editor`);
	}

	function closeLayoutEditor(): void {
		void goto(resolve('/'));
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

	function handleKeydown(event: KeyboardEvent) {
		if (isKeyboardTypingTarget(event.target) || event.metaKey || event.ctrlKey || event.altKey)
			return;
		if (event.key === 'Escape' && focusedCameraId !== null) {
			event.preventDefault();
			closeFocus();
			return;
		}
		if (event.key.toLowerCase() === 'f' && focusedCameraId !== null) {
			event.preventDefault();
			closeFocus();
			return;
		}
		if (isLayoutEditing) return;
		moveGridFocus(event);
		const target = event.target;
		if (!(target instanceof HTMLElement)) return;
		const cameraId = target.closest<HTMLElement>('[data-peek-camera]')?.dataset.peekCamera;
		if (!cameraId) return;
		if (event.key === 'Enter') {
			event.preventDefault();
			void goto(cameraHref(cameraId));
			return;
		}
		if (event.key.toLowerCase() !== 'f') return;
		event.preventDefault();
		openFocus(cameraId);
	}
</script>

<svelte:head>
	<title>Peek - KeepPeek</title>
</svelte:head>

<svelte:window onkeydown={handleKeydown} />

<div class="mx-auto max-w-[120rem] space-y-3 px-4 py-3 md:p-4">
	{#if !isLayoutEditing}
		<header class="flex min-h-10 flex-wrap items-center gap-x-3 gap-y-1">
			<h1 class="text-xl font-semibold">Peek</h1>
			<div class="ml-auto flex min-w-0 flex-wrap items-center justify-end gap-x-3 gap-y-1">
				{#if !loading && error === null && cameras.length > 0}
					<button
						type="button"
						class="hidden h-8 items-center gap-1.5 rounded-sm border border-hairline bg-raised px-2.5 text-xs font-medium text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none xl:inline-flex"
						onclick={openLayoutEditor}
					>
						<PencilIcon class="size-3.5" />
						Edit layout
					</button>
				{/if}
				<div
					data-peek-fleet-status
					class="flex shrink-0 items-center gap-2 text-xs font-medium text-muted-foreground"
					role="status"
				>
					<span class="size-1.5 rounded-full {fleetStatus.colorClass}"></span>
					<span>{fleetStatus.label}</span>
					{#if fleetStatus.showCameraCount}
						<span aria-hidden="true" class="text-border">/</span>
						<span>{cameras.length} {cameras.length === 1 ? 'camera' : 'cameras'}</span>
					{/if}
				</div>
				{#if runtimeTelemetry}
					<div
						data-peek-runtime-telemetry
						class="hidden shrink-0 items-center gap-2 border-l border-hairline pl-3 font-mono text-[10px] leading-4 text-muted-foreground sm:flex"
					>
						<span class="text-text-faint">HOST</span>
						<span>CPU {runtimeTelemetry.hostCpu}</span>
						<span>RAM {runtimeTelemetry.hostMemory}</span>
						<span aria-hidden="true" class="h-3 w-px bg-hairline"></span>
						<span class="text-text-faint">KEEPPEEK</span>
						<span>CPU {runtimeTelemetry.processCpu}</span>
						<span>RAM {runtimeTelemetry.processMemory}</span>
					</div>
				{/if}
			</div>
		</header>
	{/if}

	{#if loading}
		<div class="grid grid-cols-2 gap-2 md:grid-cols-3 2xl:grid-cols-4">
			{#each placeholders as placeholder (placeholder)}
				<Skeleton class="aspect-video w-full rounded-md" />
			{/each}
		</div>
	{:else if error}
		<div
			class="rounded-md border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
			role="alert"
		>
			{error}
		</div>
	{:else if cameras.length === 0}
		<div class="grid min-h-64 place-items-center border-y text-sm text-muted-foreground">
			No cameras configured.
		</div>
	{:else if isLayoutEditing}
		<PeekLayoutEditor
			{cameras}
			healthById={cameraHealthById}
			streamFor={previewStream}
			ondiscard={closeLayoutEditor}
		/>
	{:else}
		<div class="grid">
			{#if focusedCamera}
				<section
					data-peek-focus-return={focusReturnPending ? 'waiting' : undefined}
					class="relative z-10 col-start-1 row-start-1 space-y-3 bg-background"
					aria-label={`${cameraLabel(focusedCamera)} focus`}
					aria-busy={focusReturnPending}
				>
					<header class="flex min-h-10 flex-wrap items-center gap-2">
						<button
							type="button"
							class="grid size-9 shrink-0 place-items-center rounded-md border bg-background/40 text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							title="Return to camera grid"
							onclick={closeFocus}
						>
							<Grid2X2Icon class="size-4" />
							<span class="sr-only">Return to camera grid</span>
						</button>
						<div class="mr-auto min-w-0">
							<p class="text-[10px] font-semibold text-muted-foreground uppercase">Focus</p>
							<h2 class="truncate text-sm font-semibold text-foreground">
								{cameraLabel(focusedCamera)}
							</h2>
						</div>
						<div class="flex rounded-md border bg-background/40 p-0.5">
							<span
								class="flex h-7 items-center gap-1.5 rounded bg-foreground px-2 text-[11px] font-medium text-background"
								aria-current="page"
							>
								<RadioIcon class="size-3.5" />
								Live
							</span>
							<!-- eslint-disable svelte/no-navigation-without-resolve -->
							<a
								href={historyHref(focusedCamera.id)}
								class="flex h-7 items-center gap-1.5 rounded px-2 text-[11px] font-medium text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							>
								<HistoryIcon class="size-3.5" />
								History
							</a>
							<!-- eslint-enable svelte/no-navigation-without-resolve -->
						</div>
						<div
							class="flex rounded-md border bg-background/40 p-0.5"
							role="group"
							aria-label="Live quality ceiling"
						>
							{#each qualityOptions as quality (quality)}
								<button
									type="button"
									class="h-7 rounded px-2 text-[11px] font-medium capitalize {focusQuality ===
									quality
										? 'bg-foreground text-background'
										: 'text-muted-foreground hover:text-foreground'}"
									aria-pressed={focusQuality === quality}
									onclick={() => setFocusQuality(quality)}
								>
									{quality}
								</button>
							{/each}
						</div>
						{#if focusQualitySwitching}
							<span
								data-peek-quality-switch
								class="font-mono text-2xs text-primary-soft"
								role="status"
							>
								Switching to {pendingFocusStream === 'main' ? 'high' : 'low'} stream…
							</span>
						{/if}
						<!-- eslint-disable svelte/no-navigation-without-resolve -->
						<a
							href={cameraHref(focusedCamera.id)}
							class="grid size-9 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							title={`Open ${cameraLabel(focusedCamera)} camera information`}
						>
							<CameraIcon class="size-4" />
							<span class="sr-only">Open camera information</span>
						</a>
						<!-- eslint-enable svelte/no-navigation-without-resolve -->
					</header>

					<div class="space-y-2">
						<div data-peek-focus-history class="group relative min-w-0 self-start">
							{#key focusedCamera.id}
								<LiveVideo
									cameraId={focusedCamera.id}
									stream="main"
									quality={focusQuality}
									matchVideoAspectRatio
									onvisibilitychange={handleTileVisibility}
									class="aspect-video overflow-hidden rounded-md ring-1 ring-white/10"
								/>
							{/key}
							<span
								class="pointer-events-none absolute top-3 left-3 z-20 max-w-[calc(100%-4rem)] truncate rounded-sm bg-black/72 px-2 py-1 text-xs font-semibold text-white shadow-sm backdrop-blur-sm"
							>
								{cameraLabel(focusedCamera)}
							</span>
						</div>

						<aside class="flex min-w-0 gap-2 overflow-x-auto pb-1" aria-label="Other cameras">
							{#each filmstripCameras as camera (camera.id)}
								<article class="relative aspect-video w-40 shrink-0 overflow-hidden rounded-md">
									<LiveVideo
										cameraId={camera.id}
										stream="sub"
										onvisibilitychange={handleTileVisibility}
										class="size-full overflow-hidden ring-1 ring-white/10"
									/>
									<button
										type="button"
										class="absolute inset-0 z-10 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
										aria-label={`Focus ${cameraLabel(camera)}`}
										onclick={() => openFocus(camera.id)}
									></button>
									<span
										class="pointer-events-none absolute right-1.5 bottom-1.5 left-1.5 z-20 truncate rounded-sm bg-black/72 px-1.5 py-1 text-[10px] font-semibold text-white shadow-sm backdrop-blur-sm"
									>
										{cameraLabel(camera)}
									</span>
								</article>
							{/each}
						</aside>
					</div>
				</section>
			{/if}
			{#if focusedCamera === null || focusReturnPending}
				<div
					data-peek-wall
					data-peek-wall-state={wallRevealed ? 'ready' : 'staging'}
					data-peek-wall-reveal={wallRevealState === 'staging' ? undefined : wallRevealState}
					data-peek-wall-ready-count={wallFrameCameraIds.size}
					data-peek-wall-target-count={wallTargetCameraIds.length}
					class="relative col-start-1 row-start-1 {focusedCamera === null
						? ''
						: 'pointer-events-none opacity-0'}"
					aria-busy={!wallRevealed}
					aria-hidden={focusedCamera !== null}
				>
					<div
						data-peek-wall-content
						inert={!wallRevealed}
						class="grid grid-cols-2 gap-2.5 transition-[opacity,transform] duration-500 ease-out motion-reduce:transform-none motion-reduce:transition-none md:grid-cols-3 2xl:grid-cols-4 {wallRevealed
							? 'translate-y-0 opacity-100'
							: 'pointer-events-none translate-y-5 opacity-0'}"
					>
						{#each cameras as camera, cameraIndex (camera.id)}
							<PeekCameraTile
								{camera}
								health={cameraHealth(camera.id)}
								stream={previewStream(camera)}
								mobileFeatured={cameraIndex === 0}
								onframeactivitychange={handleWallFrameActivity}
								onvisibilitychange={handleTileVisibility}
								onfocus={openFocus}
							/>
						{/each}
					</div>
					<div
						data-peek-wall-placeholder
						class="pointer-events-none absolute inset-0 grid grid-cols-2 gap-2.5 transition-[opacity,transform] duration-300 ease-out motion-reduce:transform-none motion-reduce:transition-none md:grid-cols-3 2xl:grid-cols-4 {wallRevealed
							? '-translate-y-3 opacity-0'
							: 'translate-y-0 opacity-100'}"
						aria-hidden="true"
					>
						{#each cameras as camera, cameraIndex (camera.id)}
							<Skeleton
								class="w-full rounded-lg {cameraIndex === 0
									? 'col-span-2 aspect-video md:col-span-1'
									: 'aspect-[174/110] md:aspect-video'}"
							/>
						{/each}
					</div>
				</div>
			{/if}
		</div>
	{/if}
</div>
