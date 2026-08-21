<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { onMount, tick } from 'svelte';
	import type {
		CameraHealth,
		CameraListItem,
		Health,
		LiveQuality,
		ServerHealthResponse
	} from '$lib/types';
	import { useControlClient } from '$lib/control-context';
	import { useLivePeer } from '$lib/stream-peer-context';
	import LiveVideo from '$lib/components/LiveVideo.svelte';
	import PeekCameraTile from '$lib/components/PeekCameraTile.svelte';
	import PeekLayoutEditor from '$lib/components/PeekLayoutEditor.svelte';
	import { presentPeekCamera } from '$lib/peek-camera';
	import { isKeyboardTypingTarget } from '$lib/keyboard-shortcuts';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import RadioIcon from '@lucide/svelte/icons/radio';

	const placeholders = [0, 1, 2, 3, 4, 5, 6, 7, 8] as const;
	const qualityOptions = ['auto', 'high', 'low'] as const;
	const controlClient = useControlClient();
	const livePeer = useLivePeer();

	let health = $state.raw<Health | null>(null);
	let serverHealth = $state.raw<ServerHealthResponse | null>(null);
	let cameras = $state.raw<CameraListItem[]>([]);
	let error: string | null = $state(null);
	let loading = $state(true);
	let focusedCameraId: string | null = $state(null);
	let requestedCameraId = $derived(page.url.searchParams.get('camera')?.trim() ?? '');
	let isLayoutEditing = $derived(page.url.searchParams.get('mode') === 'layout-editor');
	let focusQuality = $state<LiveQuality>('auto');
	let focusedCamera = $derived(
		focusedCameraId === null
			? null
			: (cameras.find((camera) => camera.id === focusedCameraId) ?? null)
	);
	let filmstripCameras = $derived(
		focusedCameraId === null ? [] : cameras.filter((camera) => camera.id !== focusedCameraId)
	);
	let cameraHealthById = $derived(
		new Map((serverHealth?.cameras ?? []).map((camera) => [camera.id, camera]))
	);
	let livePlans = $derived(
		cameras
			.filter(
				(camera) =>
					camera.profiles.length > 0 &&
					presentPeekCamera(camera, cameraHealthById.get(camera.id) ?? null).state !== 'offline'
			)
			.map((camera) => ({
				cameraId: camera.id,
				quality: focusedCameraId === camera.id ? focusQuality : ('low' as const)
			}))
	);

	$effect(() => {
		if (loading) return;
		void livePeer.configure(livePlans).catch((error) => {
			console.error('Unable to configure shared live view', error);
		});
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
		loadDashboard();
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
			health = { status: nextServerHealth.status === 'healthy' ? 'ok' : 'degraded' };
			cameras = nextCameras;
			serverHealth = nextServerHealth;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to load dashboard';
		} finally {
			loading = false;
			await tick();
			if (livePeer.peekReviewTransitionActive) livePeer.finishPeekReviewTransition();
		}
	}

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function cameraHealth(cameraId: string): CameraHealth | null {
		return cameraHealthById.get(cameraId) ?? null;
	}

	function openFocus(cameraId: string) {
		focusedCameraId = cameraId;
		focusQuality = 'auto';
	}

	function closeFocus() {
		const previousCameraId = focusedCameraId;
		focusedCameraId = null;
		focusQuality = 'auto';
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

	async function openRewind(cameraId: string, targetTimestampMs: number): Promise<void> {
		const targetDate = new Date(targetTimestampMs).toISOString().slice(0, 10);
		const search = new URLSearchParams({
			camera: cameraId,
			stream: 'main',
			date: targetDate,
			at: String(Math.round(targetTimestampMs)),
			from: 'peek'
		});
		livePeer.beginPeekReviewTransition();
		await tick();
		try {
			await goto(`${resolve('/keep')}?${search}`);
		} catch (cause) {
			livePeer.finishPeekReviewTransition();
			throw cause;
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
		<header class="flex min-h-10 items-center justify-between gap-3">
			<h1 class="text-xl font-semibold">Peek</h1>
			<div class="flex items-center gap-3">
				{#if !loading && error === null && cameras.length > 0}
					<button
						type="button"
						class="hidden h-8 items-center gap-1.5 rounded-sm border border-hairline bg-raised px-2.5 text-xs font-medium text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:inline-flex"
						onclick={openLayoutEditor}
					>
						<PencilIcon class="size-3.5" />
						Edit layout
					</button>
				{/if}
				<div
					class="flex items-center gap-2 text-xs font-medium text-muted-foreground"
					role="status"
				>
					<span
						class="size-1.5 rounded-full {health?.status === 'ok'
							? 'bg-emerald-500'
							: 'bg-destructive'}"
					></span>
					<span>{health?.status === 'ok' ? 'System online' : 'System unavailable'}</span>
					<span aria-hidden="true" class="text-border">/</span>
					<span>{cameras.length} {cameras.length === 1 ? 'camera' : 'cameras'}</span>
				</div>
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
	{:else if focusedCamera}
		<section class="space-y-3" aria-label={`${cameraLabel(focusedCamera)} focus`}>
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
							class="h-7 rounded px-2 text-[11px] font-medium capitalize {focusQuality === quality
								? 'bg-foreground text-background'
								: 'text-muted-foreground hover:text-foreground'}"
							aria-pressed={focusQuality === quality}
							onclick={() => (focusQuality = quality)}
						>
							{quality}
						</button>
					{/each}
				</div>
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

			<div class="grid min-h-0 gap-2 lg:grid-cols-[minmax(0,1fr)_15rem]">
				<div class="relative min-w-0 self-start">
					{#key focusedCamera.id}
						<LiveVideo
							cameraId={focusedCamera.id}
							stream="main"
							quality={focusQuality}
							class="aspect-video overflow-hidden rounded-md ring-1 ring-white/10"
						/>
					{/key}
					<span
						class="pointer-events-none absolute top-3 left-3 z-20 max-w-[calc(100%-4rem)] truncate rounded-sm bg-black/72 px-2 py-1 text-xs font-semibold text-white shadow-sm backdrop-blur-sm"
					>
						{cameraLabel(focusedCamera)}
					</span>
				</div>

				<aside
					class="flex min-w-0 gap-2 overflow-x-auto pb-1 lg:max-h-[calc(100svh-9.5rem)] lg:flex-col lg:overflow-x-hidden lg:overflow-y-auto lg:pr-1 lg:pb-0"
					aria-label="Other cameras"
				>
					{#each filmstripCameras as camera (camera.id)}
						<article
							class="relative aspect-video w-40 shrink-0 overflow-hidden rounded-md lg:w-full"
						>
							<LiveVideo
								cameraId={camera.id}
								stream={previewStream(camera)}
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
	{:else}
		<div class="grid grid-cols-2 gap-2.5 md:grid-cols-3 2xl:grid-cols-4">
			{#each cameras as camera, cameraIndex (camera.id)}
				<PeekCameraTile
					{camera}
					health={cameraHealth(camera.id)}
					stream={previewStream(camera)}
					mobileFeatured={cameraIndex === 0}
					onfocus={openFocus}
					onrewind={openRewind}
				/>
			{/each}
		</div>
	{/if}
</div>
