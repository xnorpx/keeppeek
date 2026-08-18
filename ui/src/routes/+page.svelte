<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount } from 'svelte';
	import type { CameraListItem, Health, LiveQuality } from '$lib/types';
	import { getHealth, getCameras } from '$lib/api';
	import {
		createDefaultPeekLayoutState,
		createPeekLayout,
		createPeekLayoutId,
		layoutSlotPlacement,
		loadPeekLayoutState,
		normalizePeekLayoutState,
		orderedDynamicCameraIds,
		savePeekLayoutState,
		slotCountForLayout,
		slotsForLayout,
		type PeekLayout,
		type PeekLayoutState
	} from '$lib/peek-layouts';
	import { useLivePeer } from '$lib/stream-peer-context';
	import LiveVideo from '$lib/components/LiveVideo.svelte';
	import PeekLayoutEditor from '$lib/components/PeekLayoutEditor.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import RadioIcon from '@lucide/svelte/icons/radio';

	const placeholders = [0, 1, 2, 3, 4, 5, 6, 7, 8] as const;
	const qualityOptions = ['auto', 'high', 'low'] as const;
	const livePeer = useLivePeer();

	type LiveTile = { camera: CameraListItem; stream: 'main' | 'sub' };

	let health = $state.raw<Health | null>(null);
	let cameras = $state.raw<CameraListItem[]>([]);
	let error: string | null = $state(null);
	let loading = $state(true);
	let layoutState = $state.raw<PeekLayoutState>(createDefaultPeekLayoutState());
	let editingLayout = $state.raw<PeekLayout | null>(null);
	let layoutsLoaded = $state(false);
	let focusedCameraId: string | null = $state(null);
	let requestedCameraId = $derived(page.url.searchParams.get('camera')?.trim() ?? '');
	let focusQuality = $state<LiveQuality>('auto');
	let activeLayout = $derived(
		layoutState.layouts.find((layout) => layout.id === layoutState.activeLayoutId) ??
			layoutState.layouts[0] ??
			null
	);
	let layoutSlots = $derived.by<Array<LiveTile | null>>(() => {
		if (!activeLayout) return [];
		if (activeLayout.mode === 'dynamic') {
			const camerasById = new Map(cameras.map((camera) => [camera.id, camera]));
			return orderedDynamicCameraIds(
				activeLayout,
				cameras.map((camera) => camera.id)
			).flatMap((cameraId) => {
				const camera = camerasById.get(cameraId);
				return camera ? [{ camera, stream: previewStream(camera) }] : [];
			});
		}
		if (activeLayout.mode === 'matrix') {
			return Array.from({ length: activeLayout.rows * activeLayout.columns }, (_, index) => {
				const camera = cameras[index];
				return camera ? { camera, stream: previewStream(camera) } : null;
			});
		}
		return slotsForLayout(activeLayout).map((slot) => {
			if (!slot) return null;
			const camera = cameras.find((candidate) => candidate.id === slot.cameraId);
			return camera ? { camera, stream: slot.stream } : null;
		});
	});
	let visibleTiles = $derived.by<LiveTile[]>(() =>
		layoutSlots.filter((tile): tile is LiveTile => tile !== null)
	);
	let focusedTile = $derived(
		focusedCameraId === null
			? null
			: (visibleTiles.find((tile) => tile.camera.id === focusedCameraId) ?? null)
	);
	let focusedStream = $derived(
		activeLayout?.mode === 'custom' ? (focusedTile?.stream ?? 'main') : 'main'
	);
	let filmstripTiles = $derived(
		focusedCameraId === null
			? []
			: visibleTiles.filter((tile) => tile.camera.id !== focusedCameraId)
	);
	let livePlans = $derived.by(() => {
		const plans = new Map<string, { cameraId: string; quality: LiveQuality }>();
		for (const tile of visibleTiles) {
			plans.set(tile.camera.id, {
				cameraId: tile.camera.id,
				quality:
					focusedCameraId === tile.camera.id
						? focusQuality
						: activeLayout?.mode === 'custom'
							? qualityForStream(tile.stream)
							: 'low'
			});
		}
		if (editingLayout?.mode === 'custom') {
			for (const camera of cameras) {
				if (!plans.has(camera.id)) plans.set(camera.id, { cameraId: camera.id, quality: 'low' });
			}
		}
		return [...plans.values()];
	});
	let fixedGridStyle = $derived(
		activeLayout && activeLayout.mode !== 'dynamic'
			? activeLayout.mode === 'custom'
				? customGridStyle(activeLayout)
				: `grid-template-columns: repeat(${activeLayout.columns}, minmax(0, 1fr)); min-width: ${Math.max(18, activeLayout.columns * 8)}rem;`
			: ''
	);
	let editingExistingLayout = $derived(
		layoutState.layouts.some((layout) => layout.id === editingLayout?.id)
	);
	let canDeleteEditingLayout = $derived(editingExistingLayout && layoutState.layouts.length > 1);

	$effect(() => {
		void livePeer.configure(livePlans).catch((error) => {
			console.error('Unable to configure shared live view', error);
		});
	});

	$effect(() => {
		if (
			layoutsLoaded &&
			requestedCameraId &&
			focusedCameraId !== requestedCameraId &&
			visibleTiles.some((tile) => tile.camera.id === requestedCameraId)
		) {
			openFocus(requestedCameraId);
		}
	});

	onMount(() => {
		layoutState = loadPeekLayoutState(browserStorage());
		layoutsLoaded = true;
		void loadDashboard();
	});

	function browserStorage(): Storage | null {
		try {
			return window.localStorage;
		} catch {
			return null;
		}
	}

	function saveLayoutState(nextState: PeekLayoutState) {
		const normalized = normalizePeekLayoutState(nextState);
		layoutState = normalized;
		savePeekLayoutState(browserStorage(), normalized);
	}

	function previewStream(camera: CameraListItem): 'main' | 'sub' {
		return (
			camera.profiles.find((profile) => profile.stream === 'sub' && profile.encoding === 'h264')
				?.stream ??
			camera.profiles.find((profile) => profile.encoding === 'h264')?.stream ??
			camera.profiles.at(-1)?.stream ??
			'main'
		);
	}

	function qualityForStream(stream: 'main' | 'sub'): LiveQuality {
		return stream === 'main' ? 'auto' : 'low';
	}

	function customGridStyle(layout: PeekLayout): string {
		return `grid-template-columns: repeat(${layout.columns}, minmax(0, 1fr)); grid-template-rows: repeat(${layout.rows}, minmax(0, 1fr)); aspect-ratio: ${layout.columns * 16} / ${layout.rows * 9}; min-width: ${Math.max(24, layout.columns * 9)}rem;`;
	}

	function layoutSlotStyle(layout: PeekLayout | null, index: number): string {
		if (!layout || layout.mode !== 'custom') return '';
		const placement = layoutSlotPlacement(layout, index);
		return `grid-column: ${placement.column} / span ${placement.columnSpan}; grid-row: ${placement.row} / span ${placement.rowSpan};`;
	}

	function copyLayout(layout: PeekLayout): PeekLayout {
		return {
			...layout,
			slots: layout.slots.map((slot) => (slot ? { ...slot } : null)),
			dynamicSlots: layout.dynamicSlots?.map((slot) => ({ ...slot }))
		};
	}

	async function loadDashboard() {
		try {
			const [nextHealth, nextCameras] = await Promise.all([getHealth(), getCameras()]);
			health = nextHealth;
			cameras = nextCameras;
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
		const tile = visibleTiles.find((candidate) => candidate.camera.id === cameraId);
		if (!tile) return;
		focusedCameraId = cameraId;
		focusQuality = activeLayout?.mode === 'custom' ? qualityForStream(tile.stream) : 'auto';
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

	function selectLayout(event: Event) {
		const activeLayoutId = (event.currentTarget as HTMLSelectElement).value;
		if (!layoutState.layouts.some((layout) => layout.id === activeLayoutId)) return;
		saveLayoutState({ ...layoutState, activeLayoutId });
		editingLayout = null;
		closeFocus();
	}

	function editActiveLayout() {
		if (!activeLayout) return;
		editingLayout = copyLayout(activeLayout);
	}

	function createLayout() {
		const layout = createPeekLayout(createPeekLayoutId());
		editingLayout = {
			...layout,
			slots: Array.from({ length: slotCountForLayout(layout) }, (_, index) => {
				const camera = cameras[index];
				return camera ? { cameraId: camera.id, stream: previewStream(camera) } : null;
			})
		};
	}

	function saveLayout(layout: PeekLayout) {
		const layouts = editingExistingLayout
			? layoutState.layouts.map((candidate) => (candidate.id === layout.id ? layout : candidate))
			: [...layoutState.layouts, layout];
		saveLayoutState({ version: 1, activeLayoutId: layout.id, layouts });
		editingLayout = null;
		closeFocus();
	}

	function deleteActiveLayout() {
		if (!activeLayout || layoutState.layouts.length <= 1) return;
		const layouts = layoutState.layouts.filter((layout) => layout.id !== activeLayout.id);
		saveLayoutState({ version: 1, activeLayoutId: layouts[0].id, layouts });
		editingLayout = null;
		closeFocus();
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key !== 'Escape') return;
		if (editingLayout) {
			editingLayout = null;
			return;
		}
		closeFocus();
	}
</script>

<svelte:head>
	<title>Peek - KeepPeek</title>
</svelte:head>

<svelte:window onkeydown={handleKeydown} />

<div class="mx-auto max-w-[120rem] space-y-3">
	<header class="flex flex-col gap-3 border-b pb-3 xl:flex-row xl:items-end">
		<div class="flex min-h-9 items-center gap-3">
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
		</div>

		<div class="flex flex-wrap items-end gap-2 xl:ml-auto">
			<label class="grid gap-1 text-xs font-medium text-muted-foreground" for="peek-layout-select">
				View
				<select
					id="peek-layout-select"
					value={layoutState.activeLayoutId}
					class="h-9 min-w-40 rounded-md border bg-background px-3 text-sm text-foreground"
					onchange={selectLayout}
				>
					{#each layoutState.layouts as layout (layout.id)}
						<option value={layout.id}>{layout.name}</option>
					{/each}
				</select>
			</label>
			<Button variant="outline" size="icon" title="Create saved view" onclick={createLayout}>
				<PlusIcon />
				<span class="sr-only">Create saved view</span>
			</Button>
			<Button
				variant="outline"
				size="icon"
				title="Edit selected view"
				disabled={!activeLayout}
				onclick={editActiveLayout}
			>
				<PencilIcon />
				<span class="sr-only">Edit selected view</span>
			</Button>
		</div>
	</header>

	{#if editingLayout}
		{#key editingLayout.id}
			<PeekLayoutEditor
				layout={editingLayout}
				{cameras}
				onsave={saveLayout}
				oncancel={() => (editingLayout = null)}
				onremove={canDeleteEditingLayout ? deleteActiveLayout : undefined}
			/>
		{/key}
	{:else if loading}
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
	{:else if focusedTile}
		<section class="space-y-3" aria-label={`${cameraLabel(focusedTile.camera)} focus`}>
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
						{cameraLabel(focusedTile.camera)}
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
						href={historyHref(focusedTile.camera.id)}
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
					href={cameraHref(focusedTile.camera.id)}
					class="grid size-9 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					title={`Open ${cameraLabel(focusedTile.camera)} camera information`}
				>
					<CameraIcon class="size-4" />
					<span class="sr-only">Open camera information</span>
				</a>
				<!-- eslint-enable svelte/no-navigation-without-resolve -->
			</header>

			<div class="grid min-h-0 gap-2 lg:grid-cols-[minmax(0,1fr)_15rem]">
				<div class="relative min-w-0 self-start">
					{#key `${focusedTile.camera.id}-${focusedStream}`}
						<LiveVideo
							cameraId={focusedTile.camera.id}
							stream={focusedStream}
							quality={focusQuality}
							class="aspect-video overflow-hidden rounded-md ring-1 ring-white/10"
						/>
					{/key}
					<span
						class="pointer-events-none absolute top-3 left-3 z-20 max-w-[calc(100%-4rem)] truncate rounded-sm bg-black/72 px-2 py-1 text-xs font-semibold text-white shadow-sm backdrop-blur-sm"
					>
						{cameraLabel(focusedTile.camera)}
					</span>
				</div>

				<aside
					class="flex min-w-0 gap-2 overflow-x-auto pb-1 lg:max-h-[calc(100svh-9.5rem)] lg:flex-col lg:overflow-x-hidden lg:overflow-y-auto lg:pr-1 lg:pb-0"
					aria-label="Other cameras"
				>
					{#each filmstripTiles as tile (tile.camera.id)}
						<article
							class="relative aspect-video w-40 shrink-0 overflow-hidden rounded-md lg:w-full"
						>
							<LiveVideo
								cameraId={tile.camera.id}
								stream={tile.stream}
								class="size-full overflow-hidden ring-1 ring-white/10"
							/>
							<button
								type="button"
								class="absolute inset-0 z-10 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
								aria-label={`Focus ${cameraLabel(tile.camera)}`}
								onclick={() => openFocus(tile.camera.id)}
							></button>
							<span
								class="pointer-events-none absolute right-1.5 bottom-1.5 left-1.5 z-20 truncate rounded-sm bg-black/72 px-1.5 py-1 text-[10px] font-semibold text-white shadow-sm backdrop-blur-sm"
							>
								{cameraLabel(tile.camera)}
							</span>
						</article>
					{/each}
				</aside>
			</div>
		</section>
	{:else if visibleTiles.length === 0}
		<div class="grid min-h-64 place-items-center border-y px-4 text-center">
			<div class="space-y-3">
				<p class="text-sm text-muted-foreground">This view has no available streams.</p>
				<Button variant="outline" onclick={editActiveLayout}>
					<PencilIcon />
					Edit view
				</Button>
			</div>
		</div>
	{:else}
		<section aria-label={`${activeLayout?.name ?? 'Peek'} live view`}>
			<div class={activeLayout?.mode === 'dynamic' ? '' : 'overflow-x-auto pb-1'}>
				<div
					class={activeLayout?.mode === 'dynamic'
						? 'grid grid-cols-2 gap-2 md:grid-cols-3 2xl:grid-cols-4'
						: 'grid gap-2'}
					style={fixedGridStyle}
				>
					{#each layoutSlots as tile, index (`${tile?.camera.id ?? 'empty'}-${tile?.stream ?? 'none'}-${index}`)}
						{#if tile}
							<article
								class="group relative min-h-0 min-w-0"
								style={layoutSlotStyle(activeLayout, index)}
							>
								<LiveVideo
									cameraId={tile.camera.id}
									stream={tile.stream}
									class={activeLayout?.mode === 'custom'
										? 'size-full overflow-hidden rounded-md ring-1 ring-black/10'
										: 'aspect-video overflow-hidden rounded-md ring-1 ring-black/10'}
								/>
								<button
									type="button"
									class="absolute inset-0 z-10 rounded-md focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
									aria-label={`Focus ${cameraLabel(tile.camera)} live view`}
									onclick={() => openFocus(tile.camera.id)}
								></button>
								<span
									class="pointer-events-none absolute top-2 left-2 z-20 max-w-[calc(100%-3.5rem)] truncate rounded-sm bg-black/72 px-2 py-1 text-[11px] font-semibold text-white shadow-sm backdrop-blur-sm sm:text-xs"
								>
									{cameraLabel(tile.camera)}
								</span>
								<!-- eslint-disable svelte/no-navigation-without-resolve -->
								<a
									href={cameraHref(tile.camera.id)}
									class="absolute top-2 right-2 z-20 grid size-7 place-items-center rounded-sm bg-black/72 text-white shadow-sm backdrop-blur-sm hover:bg-black/90 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
									title={`Open ${cameraLabel(tile.camera)} camera information`}
								>
									<CameraIcon class="size-3.5" />
									<span class="sr-only">Open camera information</span>
								</a>
								<!-- eslint-enable svelte/no-navigation-without-resolve -->
							</article>
						{:else}
							<div
								class={activeLayout?.mode === 'custom'
									? 'size-full border bg-muted/20'
									: 'aspect-video border bg-muted/20'}
								style={layoutSlotStyle(activeLayout, index)}
								aria-hidden="true"
							></div>
						{/if}
					{/each}
				</div>
			</div>
		</section>
	{/if}
</div>
