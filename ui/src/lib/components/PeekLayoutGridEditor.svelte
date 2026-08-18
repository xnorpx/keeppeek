<script lang="ts">
	import {
		layoutSlotPlacement,
		slotsForLayout,
		type PeekLayout,
		type PeekLayoutSlot
	} from '$lib/peek-layouts';
	import type { CameraListItem } from '$lib/types';
	import LiveVideo from '$lib/components/LiveVideo.svelte';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import GripVerticalIcon from '@lucide/svelte/icons/grip-vertical';
	import XIcon from '@lucide/svelte/icons/x';

	type CameraSource = {
		camera: CameraListItem;
		streams: Array<'main' | 'sub'>;
	};

	type CameraDrag = {
		cameraId: string;
		stream: 'main' | 'sub';
		x: number;
		y: number;
	};

	type TilePointerDrag = {
		cameraId: string;
		stream: 'main' | 'sub';
		startX: number;
		startY: number;
	};

	type Props = {
		layout: PeekLayout;
		cameras: CameraListItem[];
		onchange: (layout: PeekLayout) => void;
	};

	let { layout, cameras, onchange }: Props = $props();
	let nativeDrag = $state<CameraDrag | null>(null);
	let tilePointerDrag = $state<TilePointerDrag | null>(null);
	let selectedCameraId = $state<string | null>(null);
	let slots = $derived(slotsForLayout(layout));
	let cameraSources = $derived.by<CameraSource[]>(() => {
		const assignedCameraIds = new Set(slots.flatMap((slot) => (slot ? [slot.cameraId] : [])));
		return cameras
			.filter((camera) => !assignedCameraIds.has(camera.id))
			.map((camera) => ({ camera, streams: streamOptions(camera) }));
	});
	let selectedCamera = $derived(
		selectedCameraId === null
			? null
			: (cameras.find((camera) => camera.id === selectedCameraId) ?? null)
	);

	$effect(() => {
		if (selectedCameraId !== null && !cameras.some((camera) => camera.id === selectedCameraId)) {
			selectedCameraId = null;
		}
	});

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function cameraName(cameraId: string): string {
		return cameras.find((camera) => camera.id === cameraId)?.name ?? cameraId;
	}

	function slotLabel(slot: PeekLayoutSlot): string {
		return cameras.find((camera) => camera.id === slot.cameraId)?.name ?? slot.cameraId;
	}

	function streamOptions(camera: CameraListItem): Array<'main' | 'sub'> {
		const streams = [...new Set(camera.profiles.map((profile) => profile.stream))];
		return streams.length > 0 ? streams : ['main'];
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

	function cameraStreams(cameraId: string): Array<'main' | 'sub'> {
		const camera = cameras.find((candidate) => candidate.id === cameraId);
		return camera ? streamOptions(camera) : ['main'];
	}

	function gridStyle(): string {
		return `grid-template-columns: repeat(${layout.columns}, minmax(0, 1fr)); grid-template-rows: repeat(${layout.rows}, minmax(0, 1fr)); aspect-ratio: ${layout.columns * 16} / ${layout.rows * 9}; min-width: ${Math.max(24, layout.columns * 9)}rem;`;
	}

	function slotStyle(index: number): string {
		const placement = layoutSlotPlacement(layout, index);
		return `grid-column: ${placement.column} / span ${placement.columnSpan}; grid-row: ${placement.row} / span ${placement.rowSpan};`;
	}

	function assignCameraToSlot(index: number, cameraId: string) {
		const camera = cameras.find((candidate) => candidate.id === cameraId);
		if (!camera) return;
		const sourceIndex = slots.findIndex((slot) => slot?.cameraId === camera.id);
		if (sourceIndex === index) {
			selectedCameraId = null;
			return;
		}
		const nextSlots = slots.map((slot) => (slot ? { ...slot } : null));
		const sourceSlot = sourceIndex >= 0 ? nextSlots[sourceIndex] : null;
		const target = nextSlots[index];

		if (sourceIndex >= 0) {
			nextSlots[sourceIndex] = target ? { ...target } : null;
		}

		nextSlots[index] = sourceSlot
			? { ...sourceSlot }
			: { cameraId: camera.id, stream: preferredStream(camera) };
		onchange({ ...layout, slots: nextSlots });
		selectedCameraId = null;
	}

	function setSlotStream(index: number, stream: 'main' | 'sub') {
		const slot = slots[index];
		if (!slot || slot.stream === stream || !cameraStreams(slot.cameraId).includes(stream)) return;
		onchange({
			...layout,
			slots: layout.slots.map((candidate, candidateIndex) =>
				candidateIndex === index && candidate ? { ...candidate, stream } : candidate
			)
		});
	}

	function clearSlot(index: number) {
		onchange({
			...layout,
			slots: layout.slots.map((slot, slotIndex) => (slotIndex === index ? null : slot))
		});
	}

	function beginCameraDrag(
		event: DragEvent,
		camera: CameraListItem,
		stream = preferredStream(camera)
	) {
		nativeDrag = {
			cameraId: camera.id,
			stream,
			x: event.clientX,
			y: event.clientY
		};
		selectedCameraId = camera.id;
		event.dataTransfer?.setData('text/plain', camera.id);
		if (event.dataTransfer) {
			event.dataTransfer.effectAllowed = 'move';
			const dragImage = document.createElement('canvas');
			dragImage.width = 1;
			dragImage.height = 1;
			event.dataTransfer.setDragImage(dragImage, 0, 0);
		}
	}

	function moveCameraDrag(event: DragEvent) {
		if (!nativeDrag || (event.clientX === 0 && event.clientY === 0)) return;
		nativeDrag = { ...nativeDrag, x: event.clientX, y: event.clientY };
	}

	function endCameraDrag() {
		nativeDrag = null;
	}

	function beginTilePointerDrag(
		event: PointerEvent,
		camera: CameraListItem,
		stream: 'main' | 'sub'
	) {
		if (event.button !== 0) return;
		event.preventDefault();
		tilePointerDrag = {
			cameraId: camera.id,
			stream,
			startX: event.clientX,
			startY: event.clientY
		};
	}

	function moveTilePointerDrag(event: PointerEvent) {
		if (!tilePointerDrag || (event.clientX === 0 && event.clientY === 0)) return;
		if (
			nativeDrag === null &&
			Math.hypot(event.clientX - tilePointerDrag.startX, event.clientY - tilePointerDrag.startY) < 6
		) {
			return;
		}
		nativeDrag = {
			cameraId: tilePointerDrag.cameraId,
			stream: tilePointerDrag.stream,
			x: event.clientX,
			y: event.clientY
		};
	}

	function endTilePointerDrag(event: PointerEvent) {
		if (!tilePointerDrag) return;
		if (nativeDrag) {
			const target = document.elementFromPoint(event.clientX, event.clientY);
			const targetTile = target?.closest<HTMLElement>('[data-grid-slot]');
			const targetIndex = Number(targetTile?.dataset.gridSlot);
			if (Number.isInteger(targetIndex)) assignCameraToSlot(targetIndex, nativeDrag.cameraId);
		}
		tilePointerDrag = null;
		nativeDrag = null;
		selectedCameraId = null;
	}

	function cancelTilePointerDrag() {
		tilePointerDrag = null;
		nativeDrag = null;
	}

	function allowDrop(event: DragEvent) {
		event.preventDefault();
		if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
	}

	function dropCamera(index: number, event: DragEvent) {
		event.preventDefault();
		const cameraId = event.dataTransfer?.getData('text/plain') || nativeDrag?.cameraId;
		if (cameraId) assignCameraToSlot(index, cameraId);
		endCameraDrag();
	}

	function selectCamera(cameraId: string) {
		selectedCameraId = selectedCameraId === cameraId ? null : cameraId;
	}

	function selectSlotCamera(index: number) {
		const slot = slots[index];
		if (!slot) return;
		if (selectedCameraId !== null && selectedCameraId !== slot.cameraId) {
			assignCameraToSlot(index, selectedCameraId);
			return;
		}
		selectCamera(slot.cameraId);
	}

	function placeSelectedCamera(index: number) {
		if (selectedCameraId) assignCameraToSlot(index, selectedCameraId);
	}
</script>

<svelte:window
	ondragover={moveCameraDrag}
	onpointermove={moveTilePointerDrag}
	onpointerup={endTilePointerDrag}
	onpointercancel={cancelTilePointerDrag}
/>

<section class="mt-3 min-w-0" aria-label="Layout grid editor">
	<div class="grid items-start gap-3 lg:grid-cols-[minmax(0,1fr)_13rem]">
		<section class="order-2 min-w-0 lg:order-1" aria-label="Custom view slots">
			<div class="overflow-x-auto pb-1">
				<div class="grid gap-2 rounded-md border bg-muted/15 p-2" role="list" style={gridStyle()}>
					{#each slots as slot, index (index)}
						<div
							role="listitem"
							data-grid-slot={index}
							class="relative flex min-h-0 min-w-0 flex-col justify-between overflow-hidden border bg-background/75 p-2 {selectedCameraId ===
							slot?.cameraId
								? 'ring-1 ring-primary'
								: ''}"
							style={slotStyle(index)}
							ondragover={allowDrop}
							ondrop={(event) => dropCamera(index, event)}
						>
							{#if slot}
								{@const tileCamera = cameras.find((camera) => camera.id === slot.cameraId)}
								{@const tileStreams = cameraStreams(slot.cameraId)}
								<LiveVideo
									cameraId={slot.cameraId}
									stream={slot.stream}
									quality="low"
									diagnostics={false}
									class="pointer-events-none absolute inset-0 z-0 size-full"
								/>
								<div
									class="relative z-10 flex min-w-0 items-center gap-1 rounded-sm bg-black/70 px-1.5 py-1 text-white shadow-sm backdrop-blur-sm"
								>
									<span class="truncate text-xs font-medium text-white">{slotLabel(slot)}</span>
									<span class="shrink-0 text-[10px] text-white/70 uppercase">{slot.stream}</span>
								</div>
								{#if tileCamera}
									<button
										type="button"
										data-grid-tile-drag-handle={index}
										class="absolute top-1 right-8 z-10 grid size-6 cursor-grab place-items-center rounded-sm bg-black/70 text-white/70 hover:bg-black/85 hover:text-white focus-visible:ring-2 focus-visible:ring-white/70 focus-visible:outline-none"
										aria-label={selectedCameraId !== null && selectedCameraId !== slot.cameraId
											? `Move ${cameraName(selectedCameraId)} to slot ${index + 1}`
											: `${selectedCameraId === slot.cameraId ? 'Deselect' : 'Select'} ${slotLabel(slot)} to move`}
										title="Drag or select to rearrange"
										onclick={() => selectSlotCamera(index)}
										onpointerdown={(event) => beginTilePointerDrag(event, tileCamera, slot.stream)}
									>
										<GripVerticalIcon class="size-3.5" />
									</button>
								{/if}
								<button
									type="button"
									class="absolute top-1 right-1 z-10 grid size-6 place-items-center rounded-sm bg-black/70 text-white/70 hover:bg-black/85 hover:text-white focus-visible:ring-2 focus-visible:ring-white/70 focus-visible:outline-none"
									aria-label={`Clear slot ${index + 1}`}
									title="Remove camera from tile"
									onclick={() => clearSlot(index)}
								>
									<XIcon class="size-3.5" />
								</button>
								{#if tileStreams.length > 1}
									<div
										class="relative z-10 mt-auto flex w-fit rounded-sm border border-white/15 bg-black/75 p-0.5"
										role="group"
										aria-label={`Stream for ${slotLabel(slot)}`}
									>
										{#each tileStreams as stream (stream)}
											<button
												type="button"
												class="h-6 rounded px-1.5 text-[10px] font-medium capitalize {slot.stream ===
												stream
													? 'bg-foreground text-background'
													: 'text-white/65 hover:text-white'}"
												aria-pressed={slot.stream === stream}
												aria-label={`Use ${stream} stream for ${slotLabel(slot)}`}
												onclick={() => setSlotStream(index, stream)}
											>
												{stream}
											</button>
										{/each}
									</div>
								{/if}
							{:else}
								<button
									type="button"
									class="flex min-h-0 flex-1 flex-col items-center justify-center gap-1 text-xs text-muted-foreground hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none aria-disabled:cursor-default"
									aria-disabled={!selectedCamera}
									aria-label={selectedCamera
										? `Place ${cameraLabel(selectedCamera)} in slot ${index + 1}`
										: `Empty tile ${index + 1}`}
									onclick={() => placeSelectedCamera(index)}
								>
									<CameraIcon class="size-4" />
									<span
										>{selectedCamera ? `Place ${cameraLabel(selectedCamera)}` : 'Drop camera'}</span
									>
								</button>
							{/if}
						</div>
					{/each}
				</div>
			</div>
		</section>

		<aside
			class="order-1 min-w-0 border-b pb-3 lg:order-2 lg:max-h-[34rem] lg:overflow-y-auto lg:border-b-0 lg:border-l lg:pt-0 lg:pb-0 lg:pl-3"
			aria-labelledby="camera-strip-heading"
		>
			<div class="mb-2 flex items-center gap-2">
				<h2 id="camera-strip-heading" class="mr-auto text-xs font-semibold text-muted-foreground">
					Available cameras
				</h2>
				<span class="text-[10px] font-medium text-muted-foreground">{cameraSources.length}</span>
			</div>
			<div
				class="flex gap-2 overflow-x-auto pb-1 lg:grid lg:overflow-x-hidden lg:overflow-y-visible lg:pb-0"
				data-camera-strip
			>
				{#each cameraSources as source (source.camera.id)}
					<article
						class="relative aspect-video w-40 shrink-0 overflow-hidden rounded-md border bg-black lg:w-full {selectedCameraId ===
						source.camera.id
							? 'border-primary ring-1 ring-primary/50'
							: 'border-border'}"
						data-camera-strip-preview={source.camera.id}
					>
						<LiveVideo
							cameraId={source.camera.id}
							stream={preferredStream(source.camera)}
							quality="low"
							diagnostics={false}
							class="absolute inset-0 size-full"
						/>
						<button
							type="button"
							draggable="true"
							data-camera-source={source.camera.id}
							class="absolute inset-0 z-10 {nativeDrag?.cameraId === source.camera.id
								? 'cursor-grabbing'
								: 'cursor-grab'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
							aria-pressed={selectedCameraId === source.camera.id}
							aria-label={`${selectedCameraId === source.camera.id ? 'Deselect' : 'Select'} ${cameraLabel(source.camera)}`}
							onclick={() => selectCamera(source.camera.id)}
							ondragstart={(event) => beginCameraDrag(event, source.camera)}
							ondrag={moveCameraDrag}
							ondragend={endCameraDrag}
						>
							<span class="sr-only">{cameraLabel(source.camera)}</span>
						</button>
						<div class="pointer-events-none absolute right-1.5 bottom-1.5 left-1.5 z-20">
							<span
								class="flex min-w-0 items-center gap-1.5 rounded-sm bg-black/72 px-1.5 py-1 text-xs font-medium text-white shadow-sm backdrop-blur-sm"
							>
								<CameraIcon class="size-3.5 shrink-0 text-white/70" />
								<span class="truncate">{cameraLabel(source.camera)}</span>
							</span>
							<span
								class="mt-1 flex items-center justify-between gap-2 text-[10px] font-medium text-white/75"
							>
								<span>{source.streams.length > 1 ? 'Main + Sub' : source.streams[0]}</span>
							</span>
						</div>
					</article>
				{/each}
				{#if cameraSources.length === 0}
					<p class="text-xs text-muted-foreground">No cameras are available.</p>
				{/if}
			</div>
		</aside>
	</div>

	{#if nativeDrag}
		<div
			class="pointer-events-none fixed z-50 w-52 -translate-x-1/2 -translate-y-1/2 overflow-hidden rounded-md border border-white/30 bg-black shadow-2xl ring-1 ring-black/30"
			data-camera-drag-preview={nativeDrag.cameraId}
			style={`left: ${nativeDrag.x}px; top: ${nativeDrag.y}px;`}
			aria-hidden="true"
		>
			<LiveVideo
				cameraId={nativeDrag.cameraId}
				stream={nativeDrag.stream}
				quality="low"
				diagnostics={false}
				class="aspect-video size-full"
			/>
			<span
				class="absolute right-2 bottom-2 left-2 truncate rounded-sm bg-black/75 px-2 py-1 text-xs font-medium text-white shadow-sm backdrop-blur-sm"
			>
				{cameraName(nativeDrag.cameraId)}
			</span>
		</div>
	{/if}
</section>
