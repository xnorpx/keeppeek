<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount, tick } from 'svelte';
	import { presentCameraFleetRow } from '$lib/camera-fleet';
	import CameraFleetRow from '$lib/components/CameraFleetRow.svelte';
	import CameraFleetSkeleton from '$lib/components/CameraFleetSkeleton.svelte';
	import { useControlClient } from '$lib/control-context';
	import { fixedRowWindow } from '$lib/fixed-row-virtualizer';
	import { useLivePeer } from '$lib/stream-peer-context';
	import type { CameraListItem, ServerHealthResponse } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import SearchIcon from '@lucide/svelte/icons/search';

	const rowHeight = 56;
	const virtualOptions = { rowHeight, overscan: 7, maxItems: 24 } as const;
	const controlClient = useControlClient();
	const livePeer = useLivePeer();

	let cameras = $state.raw<CameraListItem[]>([]);
	let serverHealth = $state.raw<ServerHealthResponse | null>(null);
	let error: string | null = $state(null);
	let loading = $state(true);
	let searchTerm = $state('');
	let unhealthyOnly = $state(false);
	let selectedIds = $state.raw<ReadonlySet<string>>(new Set());
	let focusedCameraId = $state<string | null>(null);
	let viewportElement = $state<HTMLElement | null>(null);
	let viewportHeight = $state(560);
	let scrollTop = $state(0);
	let healthById = $derived(
		new Map((serverHealth?.cameras ?? []).map((health) => [health.id, health]))
	);
	let rows = $derived(
		cameras.map((camera) => ({
			camera,
			presentation: presentCameraFleetRow(camera, healthById.get(camera.id) ?? null)
		}))
	);
	let unhealthyCount = $derived(rows.filter((row) => row.presentation.state !== 'healthy').length);
	let normalizedSearch = $derived(searchTerm.trim().toLocaleLowerCase());
	let filteredRows = $derived(
		rows.filter((row) => {
			const camera = row.camera;
			const matchesSearch = [camera.name, camera.id, camera.ip, camera.manufacturer, camera.model]
				.filter((value): value is string => value !== null)
				.some((value) => value.toLocaleLowerCase().includes(normalizedSearch));
			return matchesSearch && (!unhealthyOnly || row.presentation.state !== 'healthy');
		})
	);
	let rowWindow = $derived(
		fixedRowWindow(filteredRows.length, scrollTop, viewportHeight, virtualOptions)
	);
	let visibleRows = $derived(filteredRows.slice(rowWindow.startIndex, rowWindow.endIndex));
	let allFilteredSelected = $derived(
		filteredRows.length > 0 && filteredRows.every((row) => selectedIds.has(row.camera.id))
	);

	$effect(() => {
		void livePeer.configure([]).catch((cause) => {
			console.error('Unable to clear shared live view for camera fleet', cause);
		});
	});

	$effect(() => {
		const viewport = viewportElement;
		if (viewport === null) return;
		const measure = () => {
			viewportHeight = viewport.clientHeight;
		};
		measure();
		const observer = new ResizeObserver(measure);
		observer.observe(viewport);
		return () => observer.disconnect();
	});

	onMount(() => {
		void loadFleet();
	});

	async function loadFleet(): Promise<void> {
		try {
			const [camerasResult, healthResult] = await Promise.allSettled([
				controlClient.getCameras(),
				controlClient.getHealth()
			]);
			if (camerasResult.status === 'rejected') throw camerasResult.reason;
			cameras = camerasResult.value;
			serverHealth = healthResult.status === 'fulfilled' ? healthResult.value : null;
			error = null;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to load cameras';
		} finally {
			loading = false;
		}
	}

	function resetScroll(): void {
		focusedCameraId = null;
		scrollTop = 0;
		viewportElement?.scrollTo({ top: 0 });
	}

	function handleSearch(event: Event): void {
		searchTerm = (event.currentTarget as HTMLInputElement).value;
		resetScroll();
	}

	function toggleUnhealthy(): void {
		unhealthyOnly = !unhealthyOnly;
		resetScroll();
	}

	function handleScroll(event: Event): void {
		scrollTop = (event.currentTarget as HTMLElement).scrollTop;
	}

	function toggleCamera(cameraId: string, selected: boolean): void {
		const next = new Set(selectedIds);
		if (selected) next.add(cameraId);
		else next.delete(cameraId);
		selectedIds = next;
	}

	function toggleAllFiltered(): void {
		const next = new Set(selectedIds);
		for (const row of filteredRows) {
			if (allFilteredSelected) next.delete(row.camera.id);
			else next.add(row.camera.id);
		}
		selectedIds = next;
	}

	function clearSelection(): void {
		selectedIds = new Set();
	}

	async function moveFleetFocus(event: KeyboardEvent, cameraId: string): Promise<void> {
		if (event.key === ' ') {
			event.preventDefault();
			toggleCamera(cameraId, !selectedIds.has(cameraId));
			return;
		}
		if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return;
		event.preventDefault();
		const currentIndex = filteredRows.findIndex((row) => row.camera.id === cameraId);
		const nextIndex = Math.max(
			0,
			Math.min(filteredRows.length - 1, currentIndex + (event.key === 'ArrowDown' ? 1 : -1))
		);
		const next = filteredRows[nextIndex];
		if (!next) return;
		focusedCameraId = next.camera.id;
		const nextTop = nextIndex * rowHeight;
		if (nextTop < scrollTop || nextTop + rowHeight > scrollTop + viewportHeight) {
			scrollTop = Math.max(0, nextTop - Math.floor(viewportHeight / 2));
			viewportElement?.scrollTo({ top: scrollTop });
		}
		await tick();
		const nextFocusTarget = [
			...document.querySelectorAll<HTMLElement>(
				`[data-fleet-focus="${CSS.escape(next.camera.id)}"]`
			)
		].find((element) => element.getClientRects().length > 0);
		nextFocusTarget?.focus();
	}
</script>

<svelte:head>
	<title>Cameras - KeepPeek</title>
</svelte:head>

<div class="flex min-h-0 flex-col gap-3 px-4 py-3 md:p-4">
	<header class="flex min-h-10 flex-wrap items-center gap-3">
		<div class="flex items-baseline gap-3">
			<h1 class="text-xl font-semibold">Cameras</h1>
			<span class="font-mono text-2xs tracking-caps text-text-muted">
				{filteredRows.length} OF {cameras.length} SOURCES
			</span>
		</div>
		<div class="min-w-2 flex-1"></div>
		<label class="relative min-w-48 flex-1 md:max-w-[17.5rem]">
			<SearchIcon
				class="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-text-faint"
			/>
			<span class="sr-only">Search cameras</span>
			<input
				type="search"
				class="h-8 w-full rounded-sm border border-hairline bg-raised pr-3 pl-8 text-xs outline-none placeholder:text-text-faint focus:border-ring focus:ring-1 focus:ring-ring"
				placeholder="Name, address, model…"
				value={searchTerm}
				oninput={handleSearch}
			/>
		</label>
		<button
			type="button"
			class="inline-flex h-8 items-center gap-2 rounded-sm border px-2.5 text-xs {unhealthyOnly
				? 'border-live bg-live/10 text-foreground'
				: 'border-hairline-strong bg-raised text-text-muted'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			aria-pressed={unhealthyOnly}
			onclick={toggleUnhealthy}
		>
			<span class="size-1.5 rounded-full bg-live"></span>
			Not healthy
			<span class="font-mono text-2xs text-text-faint">{unhealthyCount}</span>
		</button>
		<a
			href={resolve('/cameras/new')}
			class="inline-flex h-8 items-center rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
		>
			Add camera
		</a>
	</header>

	{#if loading}
		<CameraFleetSkeleton cameraCount={cameras.length} />
	{:else if error}
		<div
			class="border-y border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
			role="alert"
		>
			{error}
		</div>
	{:else if cameras.length === 0}
		<div class="grid min-h-64 place-items-center border-y border-hairline text-center">
			<div>
				<p class="text-sm font-medium">No cameras configured.</p>
				<p class="mt-1 text-xs text-text-muted">Add a camera to begin building the fleet.</p>
			</div>
		</div>
	{:else}
		<div
			data-fleet-table-scroll
			class="min-w-0 overflow-x-hidden border-y border-hairline md:overflow-x-auto"
		>
			<div class="md:min-w-[1314px]">
				<div
					class="grid h-[44px] grid-cols-[44px_12px_minmax(0,1fr)_44px] items-center border-b border-hairline-strong font-mono text-2xs tracking-caps text-text-faint md:hidden"
				>
					<label class="relative grid size-11 cursor-pointer place-items-center">
						<input
							type="checkbox"
							class="peer absolute inset-0 size-11 cursor-pointer opacity-0"
							aria-label="Select all filtered cameras"
							checked={allFilteredSelected}
							onchange={toggleAllFiltered}
						/>
						<span
							class="pointer-events-none grid size-[13px] place-items-center rounded-xs border border-hairline-strong bg-raised peer-focus-visible:ring-2 peer-focus-visible:ring-ring"
						>
							{#if allFilteredSelected}<CheckIcon
									class="size-3 text-primary"
									strokeWidth={3}
								/>{/if}
						</span>
					</label>
					<span></span>
					<span class="pl-2">CAMERA AND HEALTH</span>
					<span></span>
				</div>
				<div
					class="hidden h-[34px] grid-cols-[32px_20px_270px_140px_230px_150px_140px_120px_152px_60px] items-center border-b border-hairline-strong font-mono text-2xs tracking-caps text-text-faint md:grid"
				>
					<div>
						<input
							type="checkbox"
							class="size-[13px] accent-primary"
							aria-label="Select all filtered cameras"
							checked={allFilteredSelected}
							onchange={toggleAllFiltered}
						/>
					</div>
					<span></span>
					<span>CAMERA</span>
					<span>TRANSPORT</span>
					<span>STREAMS</span>
					<span>RECORDING</span>
					<span>THROUGHPUT</span>
					<span>GB / DAY</span>
					<span>LAST EVENT</span>
					<span></span>
				</div>

				{#if filteredRows.length === 0}
					<div class="grid h-56 place-items-center text-sm text-text-muted">
						No cameras match the current filters.
					</div>
				{:else}
					<div
						bind:this={viewportElement}
						data-fleet-viewport
						data-fleet-total={filteredRows.length}
						class="h-[560px] max-h-[calc(100svh-12rem)] min-h-56 overflow-y-auto"
						onscroll={handleScroll}
					>
						<div class="relative" style:height={`${rowWindow.totalHeight}px`}>
							<div
								class="absolute inset-x-0 top-0"
								style:transform={`translateY(${rowWindow.offsetTop}px)`}
							>
								{#each visibleRows as row (row.camera.id)}
									<CameraFleetRow
										camera={row.camera}
										presentation={row.presentation}
										selected={selectedIds.has(row.camera.id)}
										tabindex={focusedCameraId === row.camera.id ||
										(focusedCameraId === null && filteredRows[0]?.camera.id === row.camera.id)
											? 0
											: -1}
										onselect={(selected) => toggleCamera(row.camera.id, selected)}
										onfocus={() => (focusedCameraId = row.camera.id)}
										onkeydown={(event) => void moveFleetFocus(event, row.camera.id)}
									/>
								{/each}
							</div>
						</div>
					</div>
				{/if}
			</div>
		</div>

		<div class="flex min-h-11 flex-wrap items-center gap-3">
			{#if selectedIds.size > 0}
				<div
					class="flex items-center gap-3 rounded-md border border-hairline-strong bg-raised px-3 py-1.5"
				>
					<span class="text-xs font-medium">{selectedIds.size} selected</span>
					<button
						type="button"
						class="text-xs text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						onclick={clearSelection}
					>
						Clear
					</button>
				</div>
			{/if}
			<div class="min-w-2 flex-1"></div>
			<span class="font-mono text-2xs tracking-caps text-text-faint">
				VIRTUALISED · 56PX ROWS · RENDERS {visibleRows.length} OF {filteredRows.length}
			</span>
		</div>
	{/if}
</div>
