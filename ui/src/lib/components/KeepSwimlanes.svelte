<script lang="ts">
	import { untrack } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import { createSwimlaneWindow } from '$lib/keep-modes';
	import type { CameraListItem, RecordingEvent, RecordingSegment } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import Rows3Icon from '@lucide/svelte/icons/rows-3';

	type Props = {
		cameras: readonly CameraListItem[];
		selectedCameraId: string;
		date: string;
		anchorMs: number;
		playheadMs?: number | null;
		lanes?: readonly LaneData[];
		paperFrame?: boolean;
		onselect: (cameraId: string, timestampMs: number) => void;
	};

	type LaneData = {
		camera: CameraListItem;
		segments: RecordingSegment[];
		events: RecordingEvent[];
	};

	let {
		cameras,
		selectedCameraId,
		date,
		anchorMs,
		playheadMs = null,
		lanes,
		paperFrame = false,
		onselect
	}: Props = $props();
	const controlClient = useControlClient();
	const initialCameraIds = untrack(() => cameras.slice(0, 8).map((camera) => camera.id));
	let selectedIds = $state.raw<ReadonlySet<string>>(new Set(initialCameraIds));
	let laneData = $state.raw<LaneData[]>([]);
	let loading = $state(false);
	let requestVersion = 0;
	let selectedCameras = $derived(
		cameras.filter((camera) => selectedIds.has(camera.id)).slice(0, 8)
	);
	let window = $derived(
		createSwimlaneWindow(
			laneData.map((lane) => ({ cameraId: lane.camera.id, segments: lane.segments })),
			anchorMs
		)
	);
	const timeFormatter = new Intl.DateTimeFormat(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		hour12: false,
		timeZone: 'UTC'
	});
	let ticks = $derived(
		Array.from({ length: 5 }, (_, index) => ({
			leftPercent: index * 25,
			timestampMs: window.startMs + index * 15 * 60_000
		}))
	);

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function leftPercent(timestampMs: number): number {
		return Math.max(
			0,
			Math.min(100, ((timestampMs - window.startMs) / (window.endMs - window.startMs)) * 100)
		);
	}

	function widthPercent(startMs: number, endMs: number): number {
		return Math.max(0.3, leftPercent(endMs) - leftPercent(startMs));
	}

	function paperPlayheadPercent(timestampMs: number): number {
		return ((150 + 1_288 * (leftPercent(timestampMs) / 100)) / 1_438) * 100;
	}

	function toggleCamera(cameraId: string): void {
		const next = new Set(selectedIds);
		if (next.has(cameraId)) {
			if (next.size === 1) return;
			next.delete(cameraId);
		} else {
			if (next.size >= 8) return;
			next.add(cameraId);
		}
		selectedIds = next;
	}

	async function loadLanes(
		cameraList: readonly CameraListItem[],
		selectedDate: string,
		signal: AbortSignal,
		version: number
	): Promise<void> {
		const [recordings, loadedEvents] = await Promise.all([
			controlClient.getRecordingsForDate(
				cameraList.map((camera) => camera.id),
				selectedDate,
				signal
			),
			Promise.all(
				cameraList.map(async (camera) => {
					const events = await controlClient
						.getRecordingEvents(camera.id, selectedDate, signal)
						.catch((cause) => {
							if (signal.aborted) throw cause;
							return { events: [] as RecordingEvent[] };
						});
					return [camera.id, events.events] as const;
				})
			)
		]);
		const segmentsByCamera = new Map(
			recordings.map((response) => [response.camera_id, response.segments] as const)
		);
		const eventsByCamera = new Map(loadedEvents);
		const loaded = cameraList.map<LaneData>((camera) => ({
			camera,
			segments: segmentsByCamera.get(camera.id) ?? [],
			events: eventsByCamera.get(camera.id) ?? []
		}));
		if (!signal.aborted && version === requestVersion) laneData = loaded;
	}

	$effect(() => {
		const cameraList = selectedCameras;
		const selectedDate = date;
		const providedLanes = lanes;
		const version = ++requestVersion;
		if (providedLanes !== undefined) {
			laneData = [...providedLanes];
			loading = false;
			return;
		}
		if (!selectedDate || cameraList.length === 0) {
			laneData = [];
			loading = false;
			return;
		}

		const controller = new AbortController();
		loading = true;
		void loadLanes(cameraList, selectedDate, controller.signal, version)
			.catch(() => {})
			.finally(() => {
				if (!controller.signal.aborted && version === requestVersion) loading = false;
			});

		return () => controller.abort();
	});
</script>

<section
	data-keep-swimlanes-owner
	class={paperFrame
		? 'flex h-[363px] w-[1440px] shrink-0 flex-col gap-5 overflow-hidden border-t border-hairline pt-10 [font-synthesis:none]'
		: 'space-y-3'}
	aria-label="Shared-clock swimlanes"
>
	<header
		class={paperFrame
			? 'flex h-[34px] shrink-0 items-baseline gap-3.5'
			: 'flex flex-wrap items-center gap-3'}
	>
		{#if paperFrame}
			<h2 class="text-[28px] leading-[34px] font-semibold">Swimlanes</h2>
			<p class="text-sm leading-[18px] text-text-muted">
				Compare indexed footage and reported events on one shared camera clock.
			</p>
		{:else}
			<div class="flex items-center gap-2">
				<Rows3Icon class="size-4 text-primary-soft" />
				<h2 class="text-sm font-semibold">Swimlanes</h2>
				<span class="font-mono text-2xs tracking-caps text-text-faint">
					{selectedCameras.length} OF 8 LANES
				</span>
			</div>
			<div class="min-w-2 flex-1"></div>
			<details class="relative">
				<summary
					class="flex h-8 cursor-pointer list-none items-center rounded-sm border border-hairline-strong bg-raised px-3 text-xs focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				>
					Choose cameras
				</summary>
				<div
					class="absolute top-10 right-0 z-50 max-h-72 w-64 overflow-y-auto rounded-md border border-hairline-strong bg-surface p-1.5 shadow-lg"
				>
					{#each cameras as camera (camera.id)}
						<button
							type="button"
							class="flex min-h-9 w-full items-center gap-2 rounded-sm px-2 text-left text-xs hover:bg-raised focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-45"
							disabled={!selectedIds.has(camera.id) && selectedIds.size >= 8}
							aria-pressed={selectedIds.has(camera.id)}
							onclick={() => toggleCamera(camera.id)}
						>
							<span
								class="grid size-4 shrink-0 place-items-center rounded-xs border border-hairline-strong {selectedIds.has(
									camera.id
								)
									? 'bg-primary text-on-primary'
									: ''}"
							>
								{#if selectedIds.has(camera.id)}<CheckIcon class="size-3" />{/if}
							</span>
							<span class="truncate">{cameraLabel(camera)}</span>
						</button>
					{/each}
				</div>
			</details>
		{/if}
	</header>

	<div
		data-swimlane-scroll
		class="border border-hairline bg-surface {paperFrame
			? 'h-[212px] w-[1440px] shrink-0 overflow-hidden rounded-lg'
			: 'overflow-x-auto rounded-md'}"
	>
		<div class={paperFrame ? 'relative min-w-0' : 'min-w-[46rem]'}>
			<div class="grid h-[34px] grid-cols-[150px_minmax(0,1fr)] border-b border-hairline">
				<div
					class="flex items-center border-r border-hairline px-3 font-mono text-2xs tracking-caps text-text-faint"
				>
					CAMERA
				</div>
				<div class="relative">
					{#each ticks as tick (tick.timestampMs)}
						<span
							class="absolute top-1/2 -translate-x-1/2 -translate-y-1/2 font-mono text-2xs text-text-faint first:translate-x-0 last:-translate-x-full"
							style:left={`${tick.leftPercent}%`}
						>
							{timeFormatter.format(new Date(tick.timestampMs))}
						</span>
					{/each}
				</div>
			</div>

			{#if loading && laneData.length === 0}
				<div class="grid h-44 place-items-center text-xs text-text-muted">
					Loading camera lanes…
				</div>
			{:else}
				{#each laneData as lane (lane.camera.id)}
					{@const laneWindow = window.lanes.find((item) => item.cameraId === lane.camera.id)}
					<div
						data-swimlane={lane.camera.id}
						class="grid h-11 grid-cols-[150px_minmax(0,1fr)] border-b border-hairline last:border-b-0"
					>
						<div
							class="flex min-w-0 items-center border-r border-hairline px-3 text-xs {lane.camera
								.id === selectedCameraId
								? 'font-semibold text-foreground'
								: 'text-text-muted'}"
						>
							<span class="truncate">{cameraLabel(lane.camera)}</span>
						</div>
						<div
							class="relative mx-3 my-[15px] h-3.5 rounded-[2px] {paperFrame
								? 'bg-availability'
								: 'bg-raised'}"
						>
							{#if laneWindow}
								{#each laneWindow.availability.available as range (`${range.startMs}-${range.endMs}`)}
									<button
										type="button"
										class="absolute inset-y-0 rounded-[2px] bg-availability hover:bg-primary-soft focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
										style:left={`${leftPercent(range.startMs)}%`}
										style:width={`${widthPercent(range.startMs, range.endMs)}%`}
										aria-label={`Review ${cameraLabel(lane.camera)} at ${timeFormatter.format(new Date((range.startMs + range.endMs) / 2))} UTC`}
										onclick={() => onselect(lane.camera.id, (range.startMs + range.endMs) / 2)}
									></button>
								{/each}
								{#each laneWindow.availability.gaps as gap (`${gap.startMs}-${gap.endMs}`)}
									<span
										data-swimlane-gap
										class="pointer-events-none absolute inset-y-0 border-x border-dashed border-hairline-strong"
										style:left={`${leftPercent(gap.startMs)}%`}
										style:width={`${widthPercent(gap.startMs, gap.endMs)}%`}
										title={`No footage ${timeFormatter.format(new Date(gap.startMs))}–${timeFormatter.format(new Date(gap.endMs))}`}
									></span>
								{/each}
							{/if}
							{#each lane.events.filter((event) => event.start_time_ms >= window.startMs && event.start_time_ms <= window.endMs) as event (event.id)}
								{#if event.end_time_ms !== null && event.end_time_ms > event.start_time_ms}
									<span
										class="pointer-events-none absolute inset-y-0 rounded-[2px] bg-activity"
										style:left={`${leftPercent(event.start_time_ms)}%`}
										style:width={`${widthPercent(event.start_time_ms, event.end_time_ms)}%`}
									></span>
								{/if}
								<button
									type="button"
									data-swimlane-event={event.id}
									class="absolute top-1/2 z-20 size-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-surface bg-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
									style:left={`${leftPercent(event.start_time_ms)}%`}
									aria-label={`${cameraLabel(lane.camera)} ${event.kind} event at ${timeFormatter.format(new Date(event.start_time_ms))} UTC`}
									onclick={() => onselect(lane.camera.id, event.start_time_ms)}
								></button>
							{/each}
						</div>
					</div>
				{:else}
					<div class="grid h-44 place-items-center text-xs text-text-muted">
						No camera lanes available.
					</div>
				{/each}
			{/if}
			{#if paperFrame && playheadMs !== null}
				<span
					data-swimlane-playhead
					class="pointer-events-none absolute top-0 z-30 h-[210px] w-0.5 bg-foreground"
					style:left={`${paperPlayheadPercent(playheadMs)}%`}
				></span>
			{/if}
		</div>
	</div>
	{#if paperFrame}
		<div data-swimlane-legend class="flex h-9 w-[1440px] shrink-0 gap-6">
			<div
				class="flex w-[464px] shrink-0 items-start gap-2.5 text-xs leading-[18px] text-text-muted"
			>
				<span class="mt-0.5 h-3 w-5 shrink-0 rounded-[2px] bg-availability"></span>
				<span>Available ranges come directly from the indexed recording timeline.</span>
			</div>
			<div
				class="flex w-[464px] shrink-0 items-start gap-2.5 text-xs leading-[18px] text-text-muted"
			>
				<span class="mt-0.5 h-3 w-5 shrink-0 border-x border-dashed border-hairline-strong"></span>
				<span>Dashed intervals are explicit gaps between returned ranges.</span>
			</div>
			<div
				class="flex w-[464px] shrink-0 items-start gap-2.5 text-xs leading-[18px] text-text-muted"
			>
				<span class="mt-1 size-[9px] shrink-0 rounded-full bg-primary"></span>
				<span>Lanes are capped at eight; additional cameras remain selectable.</span>
			</div>
		</div>
	{:else}
		<p class="text-xs leading-5 text-text-muted">
			All lanes share one hour. Missing footage remains dashed, and no more than eight cameras are
			mounted at once.
		</p>
	{/if}
</section>
