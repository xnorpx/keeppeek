<script lang="ts">
	import { Popover } from 'bits-ui';
	import type { CameraListItem } from '$lib/types';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import CheckIcon from '@lucide/svelte/icons/check';
	import ChevronDownIcon from '@lucide/svelte/icons/chevron-down';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import LoaderCircleIcon from '@lucide/svelte/icons/loader-circle';
	import SearchIcon from '@lucide/svelte/icons/search';

	type CameraSwitchDirection = -1 | 1;

	type Props = {
		cameras: CameraListItem[];
		selectedCameraId: string;
		switching?: boolean;
		onselect: (cameraId: string, direction: CameraSwitchDirection) => void;
	};

	let { cameras, selectedCameraId, switching = false, onselect }: Props = $props();
	let open = $state(false);
	let query = $state('');
	let activeIndex = $state(0);
	let searchInput = $state<HTMLInputElement | null>(null);

	let selectedIndex = $derived(cameras.findIndex((camera) => camera.id === selectedCameraId));
	let selectedCamera = $derived(cameras[selectedIndex] ?? null);
	let filteredCameras = $derived.by(() => {
		const needle = query.trim().toLowerCase();
		return needle
			? cameras.filter((camera) =>
					`${camera.name ?? ''} ${camera.id} ${camera.ip}`.toLowerCase().includes(needle)
				)
			: cameras;
	});

	$effect(() => {
		if (!open) return;
		query = '';
		activeIndex = Math.max(0, selectedIndex);
		queueMicrotask(() => searchInput?.focus());
	});

	function cameraName(camera: CameraListItem | null): string {
		return camera?.name ?? camera?.ip ?? 'Camera';
	}

	function cameraAtOffset(offset: CameraSwitchDirection): CameraListItem | null {
		if (cameras.length === 0) return null;
		const index = selectedIndex < 0 ? 0 : selectedIndex;
		return cameras[(index + offset + cameras.length) % cameras.length] ?? null;
	}

	function choose(camera: CameraListItem, direction?: CameraSwitchDirection): void {
		open = false;
		if (camera.id === selectedCameraId) return;
		const nextIndex = cameras.findIndex((candidate) => candidate.id === camera.id);
		onselect(camera.id, direction ?? (nextIndex >= selectedIndex ? 1 : -1));
	}

	function step(direction: CameraSwitchDirection): void {
		const camera = cameraAtOffset(direction);
		if (camera) choose(camera, direction);
	}

	function updateQuery(event: Event): void {
		query = (event.currentTarget as HTMLInputElement).value;
		activeIndex = 0;
	}

	function handleSearchKeydown(event: KeyboardEvent): void {
		if (event.key === 'ArrowDown') {
			event.preventDefault();
			activeIndex = filteredCameras.length ? (activeIndex + 1) % filteredCameras.length : 0;
		}
		if (event.key === 'ArrowUp') {
			event.preventDefault();
			activeIndex = filteredCameras.length
				? (activeIndex - 1 + filteredCameras.length) % filteredCameras.length
				: 0;
		}
		if (event.key === 'Enter') {
			const camera = filteredCameras[activeIndex];
			if (!camera) return;
			event.preventDefault();
			choose(camera);
		}
	}
</script>

<div class="grid min-w-0 gap-1" data-camera-switcher data-selected-camera={selectedCameraId}>
	<span id="keep-camera-switcher-label" class="text-xs font-medium text-muted-foreground">
		Camera
	</span>
	<div
		class="flex h-[46px] items-stretch overflow-hidden rounded-md border bg-background shadow-xs md:h-9"
		role="group"
		aria-labelledby="keep-camera-switcher-label"
	>
		<button
			type="button"
			class="grid w-11 shrink-0 place-items-center border-r text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-40 md:w-9"
			disabled={cameras.length <= 1 || switching}
			title={`Previous camera: ${cameraName(cameraAtOffset(-1))}`}
			aria-label={`Previous camera, ${cameraName(cameraAtOffset(-1))}`}
			onclick={() => step(-1)}
		>
			<ChevronLeftIcon class="size-4" />
		</button>

		<Popover.Root bind:open>
			<Popover.Trigger
				class="flex min-w-0 flex-1 items-center justify-between gap-3 px-2.5 text-left transition-colors hover:bg-accent focus-visible:outline-none"
				disabled={!selectedCamera}
				aria-label={`Choose camera, ${cameraName(selectedCamera)}, ${Math.max(0, selectedIndex) + 1} of ${cameras.length}`}
			>
				<span class="flex min-w-0 items-center gap-2">
					{#if switching}
						<LoaderCircleIcon class="size-3.5 shrink-0 animate-spin text-primary" />
					{:else}
						<CameraIcon class="size-3.5 shrink-0 text-muted-foreground" />
					{/if}
					<span class="max-w-36 truncate text-sm font-semibold">
						{cameraName(selectedCamera)}
					</span>
				</span>
				<span class="flex shrink-0 items-center gap-1.5">
					<span class="font-mono text-2xs text-text-faint tabular-nums">
						{Math.max(0, selectedIndex) + 1} of {cameras.length}
					</span>
					<ChevronDownIcon class="size-3.5 text-muted-foreground" />
				</span>
			</Popover.Trigger>
			<Popover.Portal>
				<Popover.Content
					role="dialog"
					aria-label="Choose a Keep camera"
					side="bottom"
					align="start"
					sideOffset={6}
					collisionPadding={8}
					trapFocus={false}
					class="z-50 w-80 max-w-[calc(100vw-1rem)] overflow-hidden rounded-md border bg-popover text-popover-foreground shadow-xl"
				>
					<div class="flex h-10 items-center gap-2 border-b px-3">
						<SearchIcon class="size-3.5 shrink-0 text-muted-foreground" />
						<input
							bind:this={searchInput}
							value={query}
							type="search"
							placeholder="Find a camera..."
							class="h-full min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
							aria-label="Find a Keep camera"
							aria-controls="keep-camera-options"
							aria-activedescendant={filteredCameras[activeIndex]
								? `keep-camera-option-${activeIndex}`
								: undefined}
							oninput={updateQuery}
							onkeydown={handleSearchKeydown}
						/>
					</div>
					<div id="keep-camera-options" class="max-h-72 overflow-y-auto py-1" role="listbox">
						{#each filteredCameras as camera, index (camera.id)}
							<button
								id={`keep-camera-option-${index}`}
								data-camera-option={camera.id}
								data-camera-label={cameraName(camera)}
								type="button"
								role="option"
								aria-selected={camera.id === selectedCameraId}
								class="flex h-11 w-full items-center gap-2.5 px-3 text-left {index === activeIndex
									? 'bg-accent'
									: 'hover:bg-accent/70'}"
								onmouseenter={() => (activeIndex = index)}
								onclick={() => choose(camera)}
							>
								<span class="grid size-5 shrink-0 place-items-center text-muted-foreground">
									<CameraIcon class="size-3.5" />
								</span>
								<span class="flex min-w-0 flex-1 flex-col">
									<span class="truncate text-sm font-medium">{cameraName(camera)}</span>
									<span class="truncate font-mono text-2xs text-text-faint">{camera.ip}</span>
								</span>
								<span class="grid w-14 shrink-0 place-items-end font-mono text-2xs text-text-faint">
									{#if camera.id === selectedCameraId}
										<CheckIcon class="size-3.5 text-primary" />
									{:else}
										{cameras.findIndex((candidate) => candidate.id === camera.id) + 1} of
										{cameras.length}
									{/if}
								</span>
							</button>
						{:else}
							<p class="px-3 py-6 text-center text-sm text-muted-foreground">No cameras found.</p>
						{/each}
					</div>
				</Popover.Content>
			</Popover.Portal>
		</Popover.Root>

		<button
			type="button"
			class="grid w-11 shrink-0 place-items-center border-l text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-40 md:w-9"
			disabled={cameras.length <= 1 || switching}
			title={`Next camera: ${cameraName(cameraAtOffset(1))}`}
			aria-label={`Next camera, ${cameraName(cameraAtOffset(1))}`}
			onclick={() => step(1)}
		>
			<ChevronRightIcon class="size-4" />
		</button>
	</div>
</div>
