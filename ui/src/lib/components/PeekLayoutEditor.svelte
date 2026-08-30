<script lang="ts">
	import { untrack } from 'svelte';
	import type { GridTileVisibility } from '$lib/grid-visibility';
	import type { CameraHealth, CameraListItem } from '$lib/types';
	import {
		addPeekLayoutCamera,
		applyPeekLayoutPreset,
		createPeekLayoutDraft,
		movePeekLayoutItem,
		peekLayoutPresets,
		removePeekLayoutCamera,
		resizePeekLayoutItem,
		setPeekLayoutActivityFocus,
		setPeekLayoutPinned,
		type PeekLayoutDraft,
		type PeekLayoutItem,
		type PeekLayoutPreset,
		type PeekLayout,
		peekLayoutDraft
	} from '$lib/peek-layout';
	import CameraOffIcon from '@lucide/svelte/icons/camera-off';
	import GripVerticalIcon from '@lucide/svelte/icons/grip-vertical';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import PinIcon from '@lucide/svelte/icons/pin';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import SearchIcon from '@lucide/svelte/icons/search';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import Undo2Icon from '@lucide/svelte/icons/undo-2';
	import XIcon from '@lucide/svelte/icons/x';
	import PeekCameraTile from './PeekCameraTile.svelte';

	type Props = {
		cameras: readonly CameraListItem[];
		healthById: ReadonlyMap<string, CameraHealth>;
		streamFor: (camera: CameraListItem) => 'main' | 'sub';
		ondiscard: () => void;
		layout?: PeekLayout | null;
		persistenceAvailable?: boolean;
		saving?: boolean;
		saveError?: string | null;
		onsave?: (draft: PeekLayoutDraft) => void | Promise<void>;
		onvisibilitychange?: (visibility: GridTileVisibility) => void;
		paperFrame?: boolean;
	};

	type DragState = {
		pointerId: number;
		cameraId: string;
		clientX: number;
		clientY: number;
		column: number;
		row: number;
		draft: PeekLayoutDraft;
	};

	type ResizeState = {
		pointerId: number;
		cameraId: string;
		clientX: number;
		clientY: number;
		columnSpan: number;
		rowSpan: number;
		draft: PeekLayoutDraft;
	};

	let {
		cameras,
		healthById,
		streamFor,
		ondiscard,
		layout = null,
		persistenceAvailable = false,
		saving = false,
		saveError = null,
		onsave,
		onvisibilitychange,
		paperFrame = false
	}: Props = $props();
	const cameraIds = untrack(() => cameras.map((camera) => camera.id));
	const initialDraft = untrack(() =>
		layout === null ? createPeekLayoutDraft(cameraIds.slice(0, 3)) : peekLayoutDraft(layout)
	);
	const gridGuides = Array.from({ length: 12 }, (_, index) => index);

	let canvasElement = $state<HTMLElement | null>(null);
	let draft = $state.raw<PeekLayoutDraft>(initialDraft);
	let history = $state.raw<readonly PeekLayoutDraft[]>([]);
	let selectedCameraId = $state<string | null>(initialDraft.items[0]?.cameraId ?? null);
	let cameraFilter = $state('');
	let dragState = $state.raw<DragState | null>(null);
	let resizeState = $state.raw<ResizeState | null>(null);
	let placedCameraIds = $derived(new Set(draft.items.map((item) => item.cameraId)));
	let unplacedCameras = $derived(
		cameras.filter((camera) => {
			const label = camera.name ?? camera.id;
			return (
				!placedCameraIds.has(camera.id) &&
				label.toLocaleLowerCase().includes(cameraFilter.trim().toLocaleLowerCase())
			);
		})
	);
	let selectedItem = $derived(
		draft.items.find((item) => item.cameraId === selectedCameraId) ?? null
	);
	let selectedCamera = $derived(cameras.find((camera) => camera.id === selectedCameraId) ?? null);
	let canSave = $derived(
		persistenceAvailable && onsave !== undefined && !saving && history.length > 0
	);

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function paperGridColumn(item: PeekLayoutItem): string {
		return item.column <= 8 ? '1' : '2';
	}

	function paperGridRow(item: PeekLayoutItem): string {
		if (item.column <= 8) return '1 / span 3';
		if (item.row <= 4) return '1';
		return item.row >= 9 ? '3' : '2';
	}

	function record(next: PeekLayoutDraft): void {
		if (next === draft) return;
		history = [...history, draft];
		draft = next;
	}

	function undo(): void {
		const previous = history.at(-1);
		if (previous === undefined) return;
		history = history.slice(0, -1);
		draft = previous;
		if (!draft.items.some((item) => item.cameraId === selectedCameraId)) {
			selectedCameraId = draft.items[0]?.cameraId ?? null;
		}
	}

	function applyPreset(preset: PeekLayoutPreset): void {
		record(applyPeekLayoutPreset(draft, cameraIds, preset));
		selectedCameraId = draft.items[0]?.cameraId ?? selectedCameraId;
	}

	function addCamera(cameraId: string): void {
		const next = addPeekLayoutCamera(draft, cameraId);
		if (next === draft) return;
		record(next);
		selectedCameraId = cameraId;
	}

	function removeSelectedCamera(): void {
		if (selectedItem === null) return;
		const next = removePeekLayoutCamera(draft, selectedItem.cameraId);
		record(next);
		selectedCameraId = next.items[0]?.cameraId ?? null;
	}

	function toggleActivityFocus(): void {
		record(setPeekLayoutActivityFocus(draft, !draft.activityFocus));
	}

	function toggleSelectedPin(): void {
		if (selectedItem === null) return;
		record(setPeekLayoutPinned(draft, selectedItem.cameraId, !selectedItem.pinned));
	}

	function beginDrag(event: PointerEvent, item: PeekLayoutItem): void {
		if (event.button !== 0 || dragState !== null || resizeState !== null) return;
		selectedCameraId = item.cameraId;
		dragState = {
			pointerId: event.pointerId,
			cameraId: item.cameraId,
			clientX: event.clientX,
			clientY: event.clientY,
			column: item.column,
			row: item.row,
			draft
		};
		(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
	}

	function moveDrag(event: PointerEvent): void {
		if (dragState === null || event.pointerId !== dragState.pointerId || canvasElement === null) {
			return;
		}
		const bounds = canvasElement.getBoundingClientRect();
		const columnDelta = Math.round(((event.clientX - dragState.clientX) / bounds.width) * 12);
		const rowDelta = Math.round(((event.clientY - dragState.clientY) / bounds.height) * 12);
		draft = movePeekLayoutItem(
			dragState.draft,
			dragState.cameraId,
			dragState.column + columnDelta,
			dragState.row + rowDelta
		);
	}

	function finishDrag(event: PointerEvent): void {
		if (dragState === null || event.pointerId !== dragState.pointerId) return;
		if (draft !== dragState.draft) history = [...history, dragState.draft];
		dragState = null;
	}

	function cancelDrag(event: PointerEvent): void {
		if (dragState === null || event.pointerId !== dragState.pointerId) return;
		draft = dragState.draft;
		dragState = null;
	}

	function nudge(event: KeyboardEvent, item: PeekLayoutItem): void {
		const offsets: Partial<Record<string, readonly [number, number]>> = {
			ArrowLeft: [-1, 0],
			ArrowRight: [1, 0],
			ArrowUp: [0, -1],
			ArrowDown: [0, 1]
		};
		const offset = offsets[event.key];
		if (offset === undefined) return;
		event.preventDefault();
		selectedCameraId = item.cameraId;
		record(movePeekLayoutItem(draft, item.cameraId, item.column + offset[0], item.row + offset[1]));
	}

	function beginResize(event: PointerEvent, item: PeekLayoutItem): void {
		if (event.button !== 0 || dragState !== null || resizeState !== null) return;
		event.stopPropagation();
		selectedCameraId = item.cameraId;
		resizeState = {
			pointerId: event.pointerId,
			cameraId: item.cameraId,
			clientX: event.clientX,
			clientY: event.clientY,
			columnSpan: item.columnSpan,
			rowSpan: item.rowSpan,
			draft
		};
		(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
	}

	function moveResize(event: PointerEvent): void {
		if (
			resizeState === null ||
			event.pointerId !== resizeState.pointerId ||
			canvasElement === null
		) {
			return;
		}
		const bounds = canvasElement.getBoundingClientRect();
		const columnDelta = Math.round(((event.clientX - resizeState.clientX) / bounds.width) * 12);
		const rowDelta = Math.round(((event.clientY - resizeState.clientY) / bounds.height) * 12);
		draft = resizePeekLayoutItem(
			resizeState.draft,
			resizeState.cameraId,
			resizeState.columnSpan + columnDelta,
			resizeState.rowSpan + rowDelta
		);
	}

	function finishResize(event: PointerEvent): void {
		if (resizeState === null || event.pointerId !== resizeState.pointerId) return;
		if (draft !== resizeState.draft) history = [...history, resizeState.draft];
		resizeState = null;
	}

	function cancelResize(event: PointerEvent): void {
		if (resizeState === null || event.pointerId !== resizeState.pointerId) return;
		draft = resizeState.draft;
		resizeState = null;
	}

	function resizeWithKeyboard(event: KeyboardEvent, item: PeekLayoutItem): void {
		const offsets: Partial<Record<string, readonly [number, number]>> = {
			ArrowLeft: [-1, 0],
			ArrowRight: [1, 0],
			ArrowUp: [0, -1],
			ArrowDown: [0, 1]
		};
		const offset = offsets[event.key];
		if (offset === undefined) return;
		event.preventDefault();
		event.stopPropagation();
		record(
			resizePeekLayoutItem(
				draft,
				item.cameraId,
				item.columnSpan + offset[0],
				item.rowSpan + offset[1]
			)
		);
	}

	function save(): void {
		if (!canSave || onsave === undefined) return;
		void onsave(draft);
	}
</script>

<section
	data-peek-layout-editor
	data-peek-layout-paper-frame={paperFrame || undefined}
	class="overflow-hidden {paperFrame
		? 'h-[838px] w-[1374px] shrink-0 bg-ground [font-synthesis:none]'
		: 'rounded-lg border border-hairline bg-surface'}"
	aria-label={`Edit ${layout?.name ?? 'Front of house'} layout`}
>
	<header
		class="flex items-center border-b border-primary bg-primary-deep text-on-primary {paperFrame
			? 'h-14 w-[1374px] shrink-0 gap-3.5 px-5'
			: 'min-h-14 flex-wrap gap-3 px-4 py-2'}"
	>
		<PencilIcon class="size-4 shrink-0" strokeWidth={2} />
		<h2 class="text-sm font-semibold">Editing “{layout?.name ?? 'Front of house'}”</h2>
		<span class="rounded-full bg-white/10 px-2.5 py-1 font-mono text-2xs tracking-caps">
			12-COL SNAP
		</span>
		<div class="min-w-2 flex-1"></div>
		<button
			type="button"
			class="inline-flex items-center gap-1.5 rounded-sm bg-white/10 text-xs hover:bg-white/15 focus-visible:ring-2 focus-visible:ring-white focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-40 {paperFrame
				? 'h-7 px-[11px]'
				: 'h-8 px-3'}"
			disabled={history.length === 0}
			onclick={undo}
		>
			<Undo2Icon class="size-3.5" />
			Undo
		</button>
		<button
			type="button"
			class="inline-flex items-center gap-1.5 rounded-sm bg-white/10 text-xs hover:bg-white/15 focus-visible:ring-2 focus-visible:ring-white focus-visible:outline-none {paperFrame
				? 'h-7 px-[11px]'
				: 'h-8 px-3'}"
			onclick={ondiscard}
		>
			{#if !paperFrame}<XIcon class="size-3.5" />{/if}
			Discard
		</button>
		<button
			type="button"
			class="rounded-sm bg-on-primary px-[13px] text-xs font-semibold text-primary-deep focus-visible:ring-2 focus-visible:ring-white focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-55 {paperFrame
				? 'h-7'
				: 'h-8'}"
			disabled={!canSave}
			title={!persistenceAvailable ? 'Server layout persistence is unavailable' : undefined}
			aria-busy={saving}
			aria-live="polite"
			onclick={save}
		>
			{saving ? 'Saving…' : 'Done'}
		</button>
	</header>
	{#if saveError}
		<p
			class="border-b border-destructive/40 bg-destructive/10 px-4 py-2 text-xs text-destructive"
			role="alert"
		>
			{saveError}
		</p>
	{/if}

	<div
		data-peek-layout-body
		class={paperFrame
			? 'grid h-[784px] w-[1374px] grid-cols-[1054px_320px]'
			: 'grid min-h-[36rem] lg:grid-cols-[minmax(0,1fr)_20rem]'}
	>
		<div
			bind:this={canvasElement}
			data-peek-layout-canvas
			class="relative grid gap-3 bg-ground p-5 {paperFrame
				? 'h-[784px] min-h-0 w-[1054px] grid-cols-[668px_334px] grid-rows-3'
				: 'min-h-[36rem] grid-cols-12 grid-rows-12'}"
		>
			<div class="pointer-events-none absolute inset-5 grid grid-cols-12" aria-hidden="true">
				{#each gridGuides as guide (guide)}
					<span class="border-r border-dashed border-primary/25 last:border-r-0"></span>
				{/each}
			</div>
			{#if paperFrame}
				<div
					data-peek-layout-drop-target
					class="relative z-[5] col-start-2 row-start-2 grid place-items-center rounded-lg border-2 border-dashed border-primary bg-primary/10"
				>
					<div class="space-y-1.5 text-center">
						<p class="text-[13px] leading-4 font-semibold text-primary-soft">Drop here</p>
						<p class="font-mono text-[10px] leading-3 tracking-[0.08em] text-primary-soft">
							SNAPS TO 4 × 4
						</p>
					</div>
				</div>
			{/if}

			{#each draft.items as item (item.cameraId)}
				{@const camera = cameras.find((candidate) => candidate.id === item.cameraId)}
				<div
					data-peek-layout-item={item.cameraId}
					data-layout-column={item.column}
					data-layout-row={item.row}
					data-layout-column-span={item.columnSpan}
					data-layout-row-span={item.rowSpan}
					class="relative z-10 min-h-0 min-w-0 touch-none select-none"
					style:grid-column={paperFrame
						? paperGridColumn(item)
						: `${item.column} / span ${item.columnSpan}`}
					style:grid-row={paperFrame ? paperGridRow(item) : `${item.row} / span ${item.rowSpan}`}
				>
					{#if camera}
						<PeekCameraTile
							{camera}
							health={healthById.get(camera.id) ?? null}
							stream={streamFor(camera)}
							layoutMode
							layoutSelected={selectedCameraId === item.cameraId}
							layoutSize={`${item.columnSpan} × ${item.rowSpan} ${paperFrame ? 'COLS' : 'GRID'}`}
							onfocus={(cameraId) => (selectedCameraId = cameraId)}
							onlayoutpointerdown={(event) => beginDrag(event, item)}
							onlayoutpointermove={moveDrag}
							onlayoutpointerup={finishDrag}
							onlayoutpointercancel={cancelDrag}
							onlayoutlostpointercapture={cancelDrag}
							onlayoutkeydown={(event) => nudge(event, item)}
							{onvisibilitychange}
						/>
					{:else}
						<button
							type="button"
							data-peek-missing-camera={item.cameraId}
							class="grid size-full place-items-center rounded-lg border border-dashed border-hairline-strong bg-surface p-3 text-center focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							aria-label={`Select unavailable camera ${item.cameraId} layout tile`}
							aria-pressed={selectedCameraId === item.cameraId}
							onclick={() => (selectedCameraId = item.cameraId)}
							onkeydown={(event) => nudge(event, item)}
						>
							<span class="space-y-1">
								<CameraOffIcon class="mx-auto size-5 text-text-faint" />
								<span class="block text-xs font-medium">Camera unavailable</span>
								<span class="block font-mono text-2xs text-text-muted">{item.cameraId}</span>
							</span>
						</button>
					{/if}
					{#if selectedCameraId === item.cameraId}
						<div
							data-peek-layout-selection-hint
							class="pointer-events-none absolute inset-0 z-30 rounded-lg {paperFrame
								? 'grid place-items-center'
								: 'flex items-end justify-center pb-10'}"
						>
							{#if camera === undefined}
								<span
									class="rounded-sm bg-video/75 px-2.5 py-1.5 text-center font-mono text-2xs tracking-caps text-white/70"
								>
									MISSING · KEEP OR REMOVE
								</span>
							{:else if paperFrame}
								<span
									class="flex flex-col gap-2 text-center font-mono text-[11px] leading-[14px] tracking-caps text-white/45"
								>
									<span>SELECTED · DRAG TO MOVE · HANDLES TO RESIZE</span>
									<span class="tracking-[0.1em]">ARROW KEYS NUDGE ONE COLUMN</span>
								</span>
							{:else}
								<span
									class="rounded-sm bg-video/75 px-2.5 py-1.5 text-center font-mono text-2xs tracking-caps text-white/70"
								>
									SELECTED · DRAG TO MOVE · CORNER TO RESIZE
								</span>
							{/if}
						</div>
						<span
							class="pointer-events-none absolute -top-1 -left-1 z-40 size-2.5 rounded-[2px] border-2 border-ground bg-primary"
						></span>
						<span
							class="pointer-events-none absolute -top-1 -right-1 z-40 size-2.5 rounded-[2px] border-2 border-ground bg-primary"
						></span>
						<span
							class="pointer-events-none absolute -bottom-1 -left-1 z-40 size-2.5 rounded-[2px] border-2 border-ground bg-primary"
						></span>
						{#if camera}
							<button
								type="button"
								class="absolute -right-5 -bottom-5 z-40 grid size-10 cursor-nwse-resize touch-none place-items-center rounded-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								aria-label={`Resize ${cameraLabel(camera)} layout tile`}
								title="Drag or use arrow keys to resize"
								onpointerdown={(event) => beginResize(event, item)}
								onpointermove={moveResize}
								onpointerup={finishResize}
								onpointercancel={cancelResize}
								onlostpointercapture={cancelResize}
								onkeydown={(event) => resizeWithKeyboard(event, item)}
							>
								<span class="size-2.5 rounded-[2px] border-2 border-ground bg-primary"></span>
							</button>
						{/if}
					{/if}
				</div>
			{/each}
		</div>

		<aside
			class="border-t border-hairline bg-surface lg:border-t-0 lg:border-l {paperFrame
				? 'flex h-[784px] w-80 shrink-0 flex-col'
				: ''}"
		>
			<div
				class="space-y-2.5 border-b border-hairline p-4 {paperFrame ? 'h-[121px] shrink-0' : ''}"
			>
				<p class="font-mono text-2xs tracking-caps text-text-faint">START FROM A PRESET</p>
				<div class="flex flex-wrap gap-1.5" aria-label="Layout presets">
					{#each peekLayoutPresets as preset (preset)}
						<button
							type="button"
							class="h-8 rounded-sm border px-2.5 text-xs {draft.preset === preset
								? 'border-primary bg-primary/10 text-foreground'
								: 'border-hairline bg-raised text-text-muted hover:text-foreground'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							aria-pressed={draft.preset === preset}
							onclick={() => applyPreset(preset)}
						>
							{preset}
						</button>
					{/each}
				</div>
			</div>

			<div class="space-y-3 border-b border-hairline p-4 {paperFrame ? 'h-[196px] shrink-0' : ''}">
				<div class="flex items-center gap-3">
					<div class="min-w-0 flex-1">
						<p class="text-sm font-semibold">Activity focus</p>
						<p class="text-xs leading-5 text-text-muted">Promote whichever camera is moving.</p>
					</div>
					<button
						type="button"
						class="relative h-[22px] w-[38px] shrink-0 rounded-full transition-colors {draft.activityFocus
							? 'bg-primary'
							: 'bg-raised ring-1 ring-hairline-strong'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						role="switch"
						aria-checked={draft.activityFocus}
						aria-label="Activity focus"
						onclick={toggleActivityFocus}
					>
						<span
							class="absolute top-[3px] left-0 size-4 rounded-full bg-on-primary transition-transform {draft.activityFocus
								? 'translate-x-[19px]'
								: 'translate-x-[3px]'}"
						></span>
					</button>
				</div>
				{#if selectedItem}
					{#if selectedCamera}
						<button
							type="button"
							class="flex w-full items-center gap-2.5 rounded-md border border-hairline bg-raised px-3 py-2.5 text-left focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							aria-pressed={selectedItem.pinned}
							onclick={toggleSelectedPin}
						>
							<PinIcon class="size-3.5 shrink-0 text-primary-soft" />
							<span class="min-w-0 flex-1">
								<span class="block truncate text-xs font-medium">
									{cameraLabel(selectedCamera)}
									{selectedItem.pinned ? 'is pinned' : 'can be promoted'}
								</span>
								<span class="block text-2xs text-text-muted">
									{selectedItem.pinned
										? 'Never demoted, whatever moves'
										: 'Pin this camera in place'}
								</span>
							</span>
						</button>
					{:else}
						<p class="truncate font-mono text-2xs text-text-muted">{selectedItem.cameraId}</p>
					{/if}
					{#if !paperFrame}
						<button
							type="button"
							class="flex h-8 w-full items-center justify-center gap-1.5 rounded-sm border border-destructive/40 text-xs font-medium text-destructive hover:bg-destructive/10 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							onclick={removeSelectedCamera}
						>
							<Trash2Icon class="size-3.5" />Remove from layout
						</button>
					{/if}
				{/if}
			</div>

			<div class="space-y-2.5 p-4 {paperFrame ? 'h-[467px] shrink-0' : ''}">
				<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-text-faint">
					<span>CAMERAS</span>
					<span class="flex-1"></span>
					<span>{draft.items.length} OF {cameras.length} PLACED</span>
				</div>
				<label class="relative block">
					<SearchIcon
						class="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-text-faint"
					/>
					<span class="sr-only">Filter cameras</span>
					<input
						type="search"
						class="h-9 w-full rounded-sm border border-hairline bg-raised pr-3 pl-8 text-xs outline-none placeholder:text-text-faint focus:border-ring focus:ring-1 focus:ring-ring"
						placeholder="Filter cameras"
						bind:value={cameraFilter}
					/>
				</label>
				<div class="max-h-56 space-y-1 overflow-y-auto" aria-live="polite">
					{#each unplacedCameras as camera, index (camera.id)}
						<div class="flex min-h-10 items-center gap-2 rounded-sm bg-raised px-2.5">
							<GripVerticalIcon class="size-3.5 shrink-0 text-text-faint" />
							<span class="min-w-0 flex-1 truncate text-xs">{cameraLabel(camera)}</span>
							<button
								type="button"
								class="grid size-7 shrink-0 place-items-center rounded-sm text-text-muted hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								aria-label={`Add ${cameraLabel(camera)} to layout`}
								onclick={() => addCamera(camera.id)}
							>
								{#if paperFrame}
									{#if healthById.get(camera.id)?.state === 'offline'}
										<span class="size-1.5 rounded-full bg-live"></span>
									{:else if index === 0}
										<span class="font-mono text-[10px] leading-3 text-text-faint">DRAG IN</span>
									{/if}
								{:else}
									<PlusIcon class="size-3.5" />
								{/if}
							</button>
						</div>
					{:else}
						<p class="py-3 text-center text-xs text-text-faint">
							{cameraFilter ? 'No unplaced cameras match.' : 'All available cameras are placed.'}
						</p>
					{/each}
				</div>
			</div>
		</aside>
	</div>
</section>
