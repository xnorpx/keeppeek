<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount } from 'svelte';
	import type { CameraListItem, Health, LiveQuality, ServerHealthResponse } from '$lib/types';
	import { getHealth, getCameras, getServerHealth } from '$lib/api';
	import { useLivePeer } from '$lib/live-peer-context';
	import LiveVideo from '$lib/components/LiveVideo.svelte';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import RadioIcon from '@lucide/svelte/icons/radio';

	const placeholders = [0, 1, 2, 3, 4, 5, 6, 7, 8] as const;
	const qualityOptions = ['auto', 'high', 'low'] as const;
	const livePeer = useLivePeer();

	let health = $state.raw<Health | null>(null);
	let cameras = $state.raw<CameraListItem[]>([]);
	let serverHealth = $state.raw<ServerHealthResponse | null>(null);
	let error: string | null = $state(null);
	let loading = $state(true);
	let focusedCameraId: string | null = $state(null);
	let requestedCameraId = $derived(page.url.searchParams.get('camera')?.trim() ?? '');
	let focusQuality = $state<LiveQuality>('auto');
	let focusedCamera = $derived(
		focusedCameraId === null
			? null
			: (cameras.find((camera) => camera.id === focusedCameraId) ?? null)
	);
	let filmstripCameras = $derived(
		focusedCameraId === null ? [] : cameras.filter((camera) => camera.id !== focusedCameraId)
	);
	let liveCameraIds = $derived(
		new Set(
			(serverHealth?.cameras ?? [])
				.filter((camera) => camera.state === 'online' || camera.state === 'degraded')
				.map((camera) => camera.id)
		)
	);
	let livePlans = $derived(
		cameras
			.filter((camera) => liveCameraIds.has(camera.id))
			.map((camera) => ({
				cameraId: camera.id,
				quality: focusedCameraId === camera.id ? focusQuality : ('low' as const)
			}))
	);

	$effect(() => {
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
			const [nextHealth, nextCameras, nextServerHealth] = await Promise.all([
				getHealth(),
				getCameras(),
				getServerHealth()
			]);
			health = nextHealth;
			cameras = nextCameras;
			serverHealth = nextServerHealth;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to load dashboard';
		} finally {
			loading = false;
		}
	}

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function openFocus(cameraId: string) {
		focusedCameraId = cameraId;
		focusQuality = 'auto';
	}

	function closeFocus() {
		focusedCameraId = null;
		focusQuality = 'auto';
	}

	function cameraHref(cameraId: string): string {
		return `${resolve('/camera')}?camera=${encodeURIComponent(cameraId)}`;
	}

	function historyHref(cameraId: string): string {
		return `${resolve('/keep')}?camera=${encodeURIComponent(cameraId)}&stream=main`;
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') closeFocus();
	}
</script>

<svelte:head>
	<title>Peek - KeepPeek</title>
</svelte:head>

<svelte:window onkeydown={handleKeydown} />

<div class="mx-auto max-w-[120rem] space-y-3">
	<header class="flex min-h-10 items-center justify-between gap-3">
		<h1 class="text-xl font-semibold">Peek</h1>
		<div class="flex items-center gap-2 text-xs font-medium text-muted-foreground" role="status">
			<span
				class="size-1.5 rounded-full {health?.status === 'ok'
					? 'bg-emerald-500'
					: 'bg-destructive'}"
			></span>
			<span>{health?.status === 'ok' ? 'System online' : 'System unavailable'}</span>
			<span aria-hidden="true" class="text-border">/</span>
			<span>{cameras.length} {cameras.length === 1 ? 'camera' : 'cameras'}</span>
		</div>
	</header>

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
		<div class="grid grid-cols-2 gap-2 md:grid-cols-3 2xl:grid-cols-4">
			{#each cameras as camera (camera.id)}
				<article class="group relative min-w-0">
					<LiveVideo
						cameraId={camera.id}
						stream={previewStream(camera)}
						class="aspect-video overflow-hidden rounded-md ring-1 ring-black/10"
					/>
					<button
						type="button"
						class="absolute inset-0 z-10 rounded-md focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
						aria-label={`Focus ${cameraLabel(camera)} live view`}
						onclick={() => openFocus(camera.id)}
					></button>
					<span
						class="pointer-events-none absolute top-2 left-2 z-20 max-w-[calc(100%-3.5rem)] truncate rounded-sm bg-black/72 px-2 py-1 text-[11px] font-semibold text-white shadow-sm backdrop-blur-sm sm:text-xs"
					>
						{cameraLabel(camera)}
					</span>
					<!-- eslint-disable svelte/no-navigation-without-resolve -->
					<a
						href={cameraHref(camera.id)}
						class="absolute top-2 right-2 z-20 grid size-7 place-items-center rounded-sm bg-black/72 text-white shadow-sm backdrop-blur-sm hover:bg-black/90 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						title={`Open ${cameraLabel(camera)} camera information`}
					>
						<CameraIcon class="size-3.5" />
						<span class="sr-only">Open camera information</span>
					</a>
					<!-- eslint-enable svelte/no-navigation-without-resolve -->
				</article>
			{/each}
		</div>
	{/if}
</div>
