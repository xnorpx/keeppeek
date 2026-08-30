<script lang="ts">
	import { onMount, tick } from 'svelte';
	import type { AccessCredential } from '$lib/access';
	import type { ControlClient } from '$lib/control-client';
	import type { GridTileVisibility } from '$lib/grid-visibility';
	import {
		GridStreamScheduler,
		type GridTileDemand,
		webDecoderBudget
	} from '$lib/grid-stream-scheduler';
	import type { PeekLayoutDraft, PeekLayoutRegistry } from '$lib/peek-layout';
	import { updatePeekLayout } from '$lib/peek-layout';
	import { useLivePeer } from '$lib/stream-peer-context';
	import type { LivePeerPlan } from '$lib/stream-peer.svelte';
	import type { CameraListItem, ServerHealthResponse } from '$lib/types';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import { Button } from './ui/button/index.js';
	import { Skeleton } from './ui/skeleton/index.js';
	import PeekLayoutEditor from './PeekLayoutEditor.svelte';
	import PeekLayoutToolbar from './PeekLayoutToolbar.svelte';

	type Props = {
		controller: ControlClient;
		health: ServerHealthResponse | null;
	};

	let { controller, health }: Props = $props();
	const livePeer = useLivePeer();
	const scheduler = new GridStreamScheduler({ subscriptionSlots: 4, decoderSlots: 4 });

	let cameras = $state.raw<readonly CameraListItem[]>([]);
	let credentials = $state.raw<readonly AccessCredential[]>([]);
	let registry = $state.raw<PeekLayoutRegistry | null>(null);
	let loading = $state(true);
	let busy = $state(false);
	let editing = $state(false);
	let error = $state<string | null>(null);
	let visibility = $state.raw<Record<string, GridTileVisibility>>({});
	let plans = $state.raw<LivePeerPlan[]>([]);
	let screenActive = true;
	let schedulerTimer: number | null = null;
	let activeLayout = $derived(
		registry?.layouts.find((layout) => layout.id === registry?.activeLayoutId) ?? null
	);
	let healthById = $derived(new Map((health?.cameras ?? []).map((camera) => [camera.id, camera])));
	let audienceLabel = $derived.by(() => {
		if (!activeLayout) return 'Unavailable';
		if (activeLayout.id === 'default' || activeLayout.audience.everyone) return 'Everyone';
		const count = activeLayout.audience.credentialIds.length;
		return count === 0
			? 'Administrators only'
			: `${count} named ${count === 1 ? 'viewer' : 'viewers'}`;
	});

	$effect(() => {
		void editing;
		void activeLayout;
		queueMicrotask(reconcilePlans);
	});

	$effect(() => {
		void livePeer.configure(plans).catch((cause) => {
			console.error('Unable to configure dashboard editor previews', cause);
		});
	});

	onMount(() => {
		const capacity = webDecoderBudget(navigator.hardwareConcurrency);
		scheduler.setCapacity({ subscriptionSlots: capacity, decoderSlots: capacity });
		const onVisibility = () => {
			screenActive = document.visibilityState === 'visible';
			reconcilePlans();
		};
		document.addEventListener('visibilitychange', onVisibility);
		void load();
		return () => {
			document.removeEventListener('visibilitychange', onVisibility);
			if (schedulerTimer) clearTimeout(schedulerTimer);
			void livePeer.configure([]);
		};
	});

	async function load(): Promise<void> {
		loading = true;
		error = null;
		try {
			[cameras, registry, credentials] = await Promise.all([
				controller.getCameras(),
				controller.getPeekLayoutRegistry(),
				controller.listAccessCredentials()
			]);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Dashboards could not be loaded.';
		} finally {
			loading = false;
		}
		reconcilePlans();
	}

	async function refreshCredentials(): Promise<void> {
		credentials = await controller.listAccessCredentials();
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

	function handleVisibility(value: GridTileVisibility): void {
		visibility = { ...visibility, [value.cameraId]: value };
		reconcilePlans();
	}

	function reconcilePlans(): void {
		if (schedulerTimer) {
			clearTimeout(schedulerTimer);
			schedulerTimer = null;
		}
		if (!editing || !activeLayout) {
			plans = [];
			return;
		}
		const layoutIds = new Set(activeLayout.items.map((item) => item.cameraId));
		const candidates = cameras.filter(
			(camera) => layoutIds.has(camera.id) && camera.profiles.length > 0
		);
		const demands: GridTileDemand[] = candidates.map((camera) => ({
			cameraId: camera.id,
			visibleFraction: visibility[camera.id]?.visibleFraction ?? 0,
			distanceFromViewportPx:
				visibility[camera.id]?.distanceFromViewportPx ?? Number.POSITIVE_INFINITY,
			viewportExtentPx: visibility[camera.id]?.viewportExtentPx ?? Math.max(1, window.innerHeight),
			focused: false,
			fullscreen: false,
			selectedForAudio: false,
			screenActive,
			mode: 'live'
		}));
		const nowMs = performance.now();
		const schedule = scheduler.reconcile(demands, nowMs);
		const granted = new Set(schedule.grants.map((grant) => grant.cameraId));
		plans = candidates.map((camera) => ({
			cameraId: camera.id,
			quality: 'low',
			active: granted.has(camera.id)
		}));
		if (schedule.nextReconcileAtMs !== null) {
			schedulerTimer = window.setTimeout(
				reconcilePlans,
				Math.max(0, schedule.nextReconcileAtMs - nowMs)
			);
		}
	}

	async function persist(candidate: PeekLayoutRegistry): Promise<boolean> {
		busy = true;
		error = null;
		try {
			registry = await controller.savePeekLayoutRegistry(candidate);
			editing = false;
			await tick();
			reconcilePlans();
			return true;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Dashboard changes were not saved.';
			return false;
		} finally {
			busy = false;
		}
	}

	async function saveGrid(draft: PeekLayoutDraft): Promise<void> {
		if (!registry || !activeLayout) return;
		await persist(updatePeekLayout(registry, activeLayout.id, draft));
	}
</script>

<section
	id="dashboards"
	class="scroll-mt-4 overflow-hidden rounded-lg border border-hairline bg-surface"
	aria-labelledby="dashboards-heading"
>
	<header class="flex flex-wrap items-center gap-4 border-b border-hairline px-5 py-4">
		<div class="min-w-48 flex-1">
			<p class="font-mono text-2xs text-primary-soft">LIVE VIEWS</p>
			<h2 id="dashboards-heading" class="mt-1 text-xl font-semibold">Dashboards</h2>
		</div>
		{#if registry && activeLayout}
			<PeekLayoutToolbar
				{registry}
				{activeLayout}
				{cameras}
				{credentials}
				{busy}
				onrefreshcredentials={refreshCredentials}
				onchange={persist}
			/>
		{/if}
	</header>

	{#if loading}
		<div class="space-y-3 p-5" aria-label="Loading dashboards">
			<Skeleton class="h-9 w-72 max-w-full" />
			<Skeleton class="h-14 w-full" />
		</div>
	{:else if error && !registry}
		<div class="flex flex-wrap items-center justify-between gap-3 p-5" role="status">
			<p class="text-sm text-destructive">{error}</p>
			<Button variant="outline" size="sm" onclick={() => void load()}>
				<RefreshCwIcon class="size-3.5" /> Retry
			</Button>
		</div>
	{:else if activeLayout}
		<div class="flex flex-wrap items-center gap-3 border-b border-hairline bg-raised/30 px-5 py-3">
			<div class="min-w-0 flex-1">
				<p class="truncate text-sm font-semibold">{activeLayout.name}</p>
				<p class="mt-0.5 text-xs text-text-muted">
					{activeLayout.id === 'default' ? 'Automatic camera grid' : audienceLabel}
				</p>
			</div>
			<Button
				variant="outline"
				size="sm"
				disabled={busy || activeLayout.id === 'default'}
				onclick={() => (editing = true)}
			>
				Edit grid
			</Button>
		</div>
		{#if error && !editing}
			<p
				class="border-b border-destructive/40 bg-destructive/10 px-5 py-2 text-xs text-destructive"
				role="alert"
			>
				{error}
			</p>
		{/if}
		{#if editing}
			<div class="p-4">
				{#key activeLayout.id}
					<PeekLayoutEditor
						{cameras}
						{healthById}
						streamFor={previewStream}
						layout={activeLayout}
						persistenceAvailable
						saving={busy}
						saveError={error}
						onsave={saveGrid}
						onvisibilitychange={handleVisibility}
						ondiscard={() => (editing = false)}
					/>
				{/key}
			</div>
		{/if}
	{/if}
</section>
