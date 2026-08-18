<script lang="ts">
	import {
		DEFAULT_PEEK_CUSTOM_LAYOUT,
		PEEK_LAYOUT_PICKER_PRESETS,
		layoutSlotPlacement,
		orderedDynamicCameraIds,
		peekLayoutPreset,
		slotCountForLayout,
		slotsForLayout,
		type PeekLayout,
		type PeekLayoutPreset,
		type PeekLayoutPresetId,
		type PeekLayoutSlot
	} from '$lib/peek-layouts';
	import type { CameraListItem } from '$lib/types';
	import PeekLayoutGridEditor from '$lib/components/PeekLayoutGridEditor.svelte';
	import LiveVideo from '$lib/components/LiveVideo.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import GripVerticalIcon from '@lucide/svelte/icons/grip-vertical';
	import SaveIcon from '@lucide/svelte/icons/save';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import XIcon from '@lucide/svelte/icons/x';

	const layoutModes = [
		{ id: 'dynamic', label: 'Dynamic' },
		{ id: 'custom', label: 'Preset' }
	] as const;
	type EditorLayoutMode = (typeof layoutModes)[number]['id'];
	const customLayouts: Array<Pick<PeekLayoutPreset, 'id' | 'label' | 'cameraCount'>> =
		PEEK_LAYOUT_PICKER_PRESETS;
	type DynamicDrag = {
		cameraId: string;
		x: number;
		y: number;
	};
	type DynamicPointerDrag = {
		cameraId: string;
		startX: number;
		startY: number;
	};

	type Props = {
		layout: PeekLayout;
		cameras: CameraListItem[];
		onsave: (layout: PeekLayout) => void;
		oncancel: () => void;
		onremove?: () => void;
	};

	let { layout, cameras, onsave, oncancel, onremove }: Props = $props();
	let draft = $state.raw<PeekLayout | null>(null);
	let dynamicDrag = $state<DynamicDrag | null>(null);
	let dynamicPointerDrag = $state<DynamicPointerDrag | null>(null);
	let selectedDynamicCameraId = $state<string | null>(null);
	let dynamicCameras = $derived.by<CameraListItem[]>(() => {
		if (!draft) return [];
		const camerasById = new Map(cameras.map((camera) => [camera.id, camera]));
		return orderedDynamicCameraIds(
			draft,
			cameras.map((camera) => camera.id)
		).flatMap((cameraId) => {
			const camera = camerasById.get(cameraId);
			return camera ? [camera] : [];
		});
	});

	$effect(() => {
		if (draft?.id !== layout.id) draft = editorLayout(layout);
	});

	function copyLayout(source: PeekLayout): PeekLayout {
		return {
			...source,
			slots: source.slots.map((slot) => (slot ? { ...slot } : null))
		};
	}

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function dynamicCameraName(cameraId: string): string {
		return cameras.find((camera) => camera.id === cameraId)?.name ?? cameraId;
	}

	function preferredStream(camera: CameraListItem): 'main' | 'sub' {
		return (
			camera.profiles.find((profile) => profile.stream === 'sub' && profile.encoding === 'h264')
				?.stream ??
			camera.profiles.find((profile) => profile.encoding === 'h264')?.stream ??
			camera.profiles.at(-1)?.stream ??
			'main'
		);
	}

	function editorLayout(source: PeekLayout): PeekLayout {
		const copied = copyLayout(source);
		if (copied.mode !== 'matrix') return copied;
		const preset = peekLayoutPreset(DEFAULT_PEEK_CUSTOM_LAYOUT)!;
		return {
			...copied,
			mode: 'custom',
			customLayout: preset.id,
			rows: preset.rows,
			columns: preset.columns,
			slots: initialCustomSlots(preset.placements.length)
		};
	}

	function setMode(mode: EditorLayoutMode) {
		if (!draft || mode === draft.mode) return;
		const current = draft;
		if (mode === 'dynamic') {
			draft = { ...current, mode };
			return;
		}

		const existingPreset = peekLayoutPreset(current.customLayout);
		const preset = existingPreset ?? peekLayoutPreset(DEFAULT_PEEK_CUSTOM_LAYOUT)!;
		const nextLayout: PeekLayout = {
			...current,
			mode,
			customLayout: preset.id,
			rows: preset.rows,
			columns: preset.columns
		};
		draft = {
			...nextLayout,
			slots: existingPreset
				? Array.from(
						{ length: slotCountForLayout(nextLayout) },
						(_, index) => current.slots[index] ?? null
					)
				: initialCustomSlots(slotCountForLayout(nextLayout))
		};
	}

	function setCustomLayout(customLayout: PeekLayoutPresetId) {
		if (!draft) return;
		const current = draft;
		const preset = peekLayoutPreset(customLayout);
		const nextLayout: PeekLayout = {
			...current,
			customLayout,
			rows: preset?.rows ?? current.rows,
			columns: preset?.columns ?? current.columns
		};
		draft = {
			...nextLayout,
			slots: initialCustomSlots(slotCountForLayout(nextLayout))
		};
	}

	function initialCustomSlots(slotCount: number): Array<PeekLayoutSlot | null> {
		return Array.from({ length: slotCount }, (_, index) =>
			cameras[index]
				? { cameraId: cameras[index].id, stream: preferredStream(cameras[index]) }
				: null
		);
	}

	function setDynamicOrder(nextCameras: CameraListItem[]) {
		if (!draft) return;
		draft = {
			...draft,
			dynamicSlots: nextCameras.map((camera) => ({
				cameraId: camera.id,
				stream: preferredStream(camera)
			}))
		};
	}

	function swapDynamicCameras(sourceIndex: number, targetIndex: number) {
		if (sourceIndex === targetIndex) return;
		if (
			sourceIndex < 0 ||
			targetIndex < 0 ||
			sourceIndex >= dynamicCameras.length ||
			targetIndex >= dynamicCameras.length
		) {
			return;
		}
		const nextCameras = [...dynamicCameras];
		[nextCameras[sourceIndex], nextCameras[targetIndex]] = [
			nextCameras[targetIndex],
			nextCameras[sourceIndex]
		];
		setDynamicOrder(nextCameras);
	}

	function selectDynamicCamera(index: number) {
		const camera = dynamicCameras[index];
		if (!camera) return;
		if (selectedDynamicCameraId === null) {
			selectedDynamicCameraId = camera.id;
			return;
		}
		const sourceIndex = dynamicCameras.findIndex(
			(candidate) => candidate.id === selectedDynamicCameraId
		);
		if (sourceIndex === index) {
			selectedDynamicCameraId = null;
			return;
		}
		swapDynamicCameras(sourceIndex, index);
		selectedDynamicCameraId = null;
	}

	function beginDynamicPointerDrag(event: PointerEvent, camera: CameraListItem) {
		if (event.button !== 0) return;
		event.preventDefault();
		dynamicPointerDrag = {
			cameraId: camera.id,
			startX: event.clientX,
			startY: event.clientY
		};
	}

	function moveDynamicPointerDrag(event: PointerEvent) {
		if (!dynamicPointerDrag || (event.clientX === 0 && event.clientY === 0)) return;
		if (
			dynamicDrag === null &&
			Math.hypot(
				event.clientX - dynamicPointerDrag.startX,
				event.clientY - dynamicPointerDrag.startY
			) < 6
		) {
			return;
		}
		dynamicDrag = {
			cameraId: dynamicPointerDrag.cameraId,
			x: event.clientX,
			y: event.clientY
		};
	}

	function endDynamicPointerDrag(event: PointerEvent) {
		if (dynamicDrag) {
			const cameraId = dynamicDrag.cameraId;
			const target = document.elementFromPoint(event.clientX, event.clientY);
			const targetTile = target?.closest<HTMLElement>('[data-dynamic-editor-tile]');
			const targetCameraId = targetTile?.dataset.dynamicEditorTile;
			const sourceIndex = dynamicCameras.findIndex((camera) => camera.id === cameraId);
			const targetIndex = dynamicCameras.findIndex((camera) => camera.id === targetCameraId);
			swapDynamicCameras(sourceIndex, targetIndex);
		}
		dynamicPointerDrag = null;
		dynamicDrag = null;
		selectedDynamicCameraId = null;
	}

	function cancelDynamicPointerDrag() {
		dynamicPointerDrag = null;
		dynamicDrag = null;
	}

	function previewLayout(customLayout: PeekLayoutPresetId): PeekLayout {
		const preset = peekLayoutPreset(customLayout);
		return {
			id: 'template-preview',
			name: 'Template preview',
			mode: 'custom',
			customLayout,
			rows: preset?.rows ?? 3,
			columns: preset?.columns ?? 3,
			slots: []
		};
	}

	function previewGridStyle(customLayout: PeekLayoutPresetId): string {
		const layout = previewLayout(customLayout);
		return `grid-template-columns: repeat(${layout.columns}, minmax(0, 1fr)); grid-template-rows: repeat(${layout.rows}, minmax(0, 1fr)); aspect-ratio: 16 / 9;`;
	}

	function previewSlotCount(customLayout: PeekLayoutPresetId): number {
		return slotCountForLayout(previewLayout(customLayout));
	}

	function previewSlotStyle(customLayout: PeekLayoutPresetId, index: number): string {
		const placement = layoutSlotPlacement(previewLayout(customLayout), index);
		return `grid-column: ${placement.column} / span ${placement.columnSpan}; grid-row: ${placement.row} / span ${placement.rowSpan};`;
	}

	function updateName(event: Event) {
		if (!draft) return;
		draft = { ...draft, name: (event.currentTarget as HTMLInputElement).value };
	}

	function save(event: SubmitEvent) {
		event.preventDefault();
		if (!draft) return;
		const current = draft;
		onsave({
			...current,
			name: current.name.trim() || 'Untitled',
			slots: current.slots,
			dynamicSlots:
				current.mode === 'dynamic'
					? dynamicCameras.map((camera) => ({
							cameraId: camera.id,
							stream: preferredStream(camera)
						}))
					: current.dynamicSlots
		});
	}
</script>

<svelte:window
	onpointermove={moveDynamicPointerDrag}
	onpointerup={endDynamicPointerDrag}
	onpointercancel={cancelDynamicPointerDrag}
/>

{#if draft}
	<form class="border-y py-3" aria-label="View editor" onsubmit={save}>
		<div class="flex flex-col gap-3 lg:flex-row lg:items-end">
			<label
				class="grid min-w-44 flex-1 gap-1 text-xs font-medium text-muted-foreground"
				for="peek-view-name"
			>
				View name
				<input
					id="peek-view-name"
					value={draft.name}
					maxlength="80"
					class="h-9 rounded-md border bg-background px-3 text-sm text-foreground"
					oninput={updateName}
				/>
			</label>

			<fieldset class="grid gap-1">
				<legend class="text-xs font-medium text-muted-foreground">Layout</legend>
				<div
					class="flex rounded-md border bg-background/40 p-0.5"
					role="group"
					aria-label="Layout mode"
				>
					{#each layoutModes as mode (mode.id)}
						<button
							type="button"
							class="h-8 rounded px-2.5 text-xs font-medium {draft.mode === mode.id
								? 'bg-foreground text-background'
								: 'text-muted-foreground hover:text-foreground'}"
							aria-pressed={draft.mode === mode.id}
							onclick={() => setMode(mode.id)}
						>
							{mode.label}
						</button>
					{/each}
				</div>
			</fieldset>

			<div class="flex flex-wrap items-center gap-2 lg:ml-auto">
				{#if onremove}
					<Button
						variant="ghost"
						class="text-destructive hover:text-destructive"
						onclick={onremove}
					>
						<Trash2Icon />
						Delete
					</Button>
				{/if}
				<Button variant="outline" onclick={oncancel}>
					<XIcon />
					Cancel
				</Button>
				<Button type="submit">
					<SaveIcon />
					Save view
				</Button>
			</div>
		</div>

		{#if draft.mode === 'dynamic'}
			<section class="mt-3" aria-label="Dynamic live view preview">
				<div class="grid grid-cols-2 gap-2 md:grid-cols-3 2xl:grid-cols-4">
					{#each dynamicCameras as camera, index (camera.id)}
						<article
							class="relative aspect-video overflow-hidden border bg-black {selectedDynamicCameraId ===
							camera.id
								? 'ring-1 ring-primary'
								: ''}"
							data-dynamic-editor-tile={camera.id}
						>
							<LiveVideo
								cameraId={camera.id}
								stream={preferredStream(camera)}
								quality="low"
								diagnostics={false}
								class="pointer-events-none absolute inset-0 size-full"
							/>
							<div class="absolute top-1.5 right-1.5 z-10">
								<button
									type="button"
									data-dynamic-tile-drag-handle={index}
									class="grid size-7 cursor-grab place-items-center rounded-sm bg-black/72 text-white/70 hover:bg-black/85 hover:text-white focus-visible:ring-2 focus-visible:ring-white/70 focus-visible:outline-none"
									aria-label={selectedDynamicCameraId !== null &&
									selectedDynamicCameraId !== camera.id
										? `Move ${dynamicCameraName(selectedDynamicCameraId)} to position ${index + 1}`
										: `${selectedDynamicCameraId === camera.id ? 'Deselect' : 'Select'} ${cameraLabel(camera)} to swap`}
									title="Drag or select to swap cameras"
									onclick={() => selectDynamicCamera(index)}
									onpointerdown={(event) => beginDynamicPointerDrag(event, camera)}
								>
									<GripVerticalIcon class="size-4" />
								</button>
							</div>
							<div class="pointer-events-none absolute right-1.5 bottom-1.5 left-1.5">
								<span
									class="flex min-w-0 items-center gap-1.5 rounded-sm bg-black/72 px-1.5 py-1 text-xs font-medium text-white shadow-sm backdrop-blur-sm"
								>
									<CameraIcon class="size-3.5 shrink-0 text-white/70" />
									<span class="truncate">{cameraLabel(camera)}</span>
								</span>
							</div>
						</article>
					{/each}
				</div>
			</section>
		{:else if draft.mode === 'custom'}
			<fieldset class="mt-3 grid gap-2">
				<legend class="text-xs font-medium text-muted-foreground">Camera layout</legend>
				<div class="grid grid-cols-[repeat(auto-fit,minmax(8rem,10rem))] gap-2">
					{#each customLayouts as customLayout (customLayout.id)}
						<button
							type="button"
							class="grid min-w-0 gap-1 rounded-md border p-1.5 text-left text-xs font-medium {draft.customLayout ===
							customLayout.id
								? 'border-primary bg-primary/10 text-foreground'
								: 'bg-background text-muted-foreground hover:bg-accent hover:text-foreground'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							aria-label={`Choose ${customLayout.label} layout`}
							aria-pressed={draft.customLayout === customLayout.id}
							onclick={() => setCustomLayout(customLayout.id)}
						>
							<span
								class="grid gap-px rounded-sm border bg-muted/30 p-px"
								style={previewGridStyle(customLayout.id)}
								aria-hidden="true"
							>
								{#each Array.from( { length: previewSlotCount(customLayout.id) } ) as _, index (index)}
									<span
										class="min-h-0 min-w-0 border border-muted-foreground/40 bg-background/70"
										style={previewSlotStyle(customLayout.id, index)}
									></span>
								{/each}
							</span>
							<span class="truncate">{customLayout.label}</span>
						</button>
					{/each}
				</div>
			</fieldset>
		{/if}

		{#if draft.mode === 'custom'}
			<PeekLayoutGridEditor
				layout={draft}
				{cameras}
				onchange={(nextLayout) => (draft = nextLayout)}
			/>
		{/if}
	</form>
{/if}

{#if dynamicDrag}
	{@const draggedCamera = cameras.find((camera) => camera.id === dynamicDrag?.cameraId)}
	{#if draggedCamera}
		<div
			class="pointer-events-none fixed z-50 w-52 -translate-x-1/2 -translate-y-1/2 overflow-hidden rounded-md border border-white/30 bg-black shadow-2xl ring-1 ring-black/30"
			data-dynamic-drag-preview={draggedCamera.id}
			style={`left: ${dynamicDrag.x}px; top: ${dynamicDrag.y}px;`}
			aria-hidden="true"
		>
			<LiveVideo
				cameraId={draggedCamera.id}
				stream={preferredStream(draggedCamera)}
				quality="low"
				diagnostics={false}
				class="aspect-video size-full"
			/>
			<span
				class="absolute right-2 bottom-2 left-2 truncate rounded-sm bg-black/75 px-2 py-1 text-xs font-medium text-white shadow-sm backdrop-blur-sm"
			>
				{cameraLabel(draggedCamera)}
			</span>
		</div>
	{/if}
{/if}
