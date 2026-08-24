<script lang="ts">
	import { replaceState } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount, tick } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import EventDetailDrawer from '$lib/components/EventDetailDrawer.svelte';
	import EventNoResultsState from '$lib/components/EventNoResultsState.svelte';
	import EventResultCard from '$lib/components/EventResultCard.svelte';
	import {
		EVENT_BROWSER_INITIAL_WINDOW_MS,
		EVENT_BROWSER_PAGE_SIZE,
		eventBrowserDayBounds,
		eventBrowserRecordKey,
		eventBrowserSearchParams,
		eventFilterSummary,
		eventNoResultsSuggestion,
		filterEventBrowserRecords,
		parseEventBrowserFilters,
		previousEventBrowserWindow,
		type EventBrowserFilters,
		type EventBrowserRecord,
		type EventImageFilter,
		type EventPreviewState
	} from '$lib/event-browser';
	import type { CameraListItem, RecordingEvent } from '$lib/types';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SearchIcon from '@lucide/svelte/icons/search';
	import SlidersHorizontalIcon from '@lucide/svelte/icons/sliders-horizontal';

	const today = new Date().toISOString().slice(0, 10);
	const LIVE_REFRESH_INTERVAL_MS = 5_000;
	const eventKinds = ['all', 'person', 'vehicle', 'motion', 'story'] as const;
	const imageFilters: readonly { value: EventImageFilter; label: string }[] = [
		{ value: 'all', label: 'Any image' },
		{ value: 'with', label: 'With image' },
		{ value: 'without', label: 'No image' }
	];
	const controlClient = useControlClient();

	let cameras = $state.raw<CameraListItem[]>([]);
	let records = $state.raw<EventBrowserRecord[]>([]);
	let filters = $state.raw<EventBrowserFilters>(
		parseEventBrowserFilters(page.url.searchParams, today)
	);
	let selectedKey = $state<string | null>(null);
	let focusedKey = $state<string | null>(null);
	let loading = $state(true);
	let refreshing = $state(false);
	let refreshDelayed = $state(false);
	let currentDate = $state(today);
	let error = $state<string | null>(null);
	let requestVersion = 0;
	let loadController: AbortController | null = null;
	let refreshController: AbortController | null = null;
	let previewQueue: EventBrowserRecord[] = [];
	let activePreviewCount = 0;
	const previewKeys = new Set<string>();
	const previewControllers = new Map<string, AbortController>();
	let previewStates = $state.raw<Record<string, EventPreviewState>>({});
	const maxConcurrentPreviews = 2;
	let dayStartMs = $state(0);
	let loadedStartMs = $state(0);
	let nextWindowDurationMs = $state(EVENT_BROWSER_INITIAL_WINDOW_MS);
	let resultPage = $state(0);
	let loadingEarlier = $state(false);
	let isToday = $derived(filters.date === currentDate);
	let filteredRecords = $derived(filterEventBrowserRecords(records, filters));
	let visibleRecords = $derived(
		filteredRecords.slice(
			resultPage * EVENT_BROWSER_PAGE_SIZE,
			(resultPage + 1) * EVENT_BROWSER_PAGE_SIZE
		)
	);
	let visibleResultStart = $derived(
		filteredRecords.length === 0 ? 0 : resultPage * EVENT_BROWSER_PAGE_SIZE + 1
	);
	let visibleResultEnd = $derived(
		Math.min((resultPage + 1) * EVENT_BROWSER_PAGE_SIZE, filteredRecords.length)
	);
	let selectedRecord = $derived(
		selectedKey === null
			? null
			: (records.find((record) => eventBrowserRecordKey(record) === selectedKey) ?? null)
	);
	let availableTypes = $derived(
		[...new Set(records.map((record) => record.event.kind.toLocaleLowerCase()))].toSorted()
	);
	let noResultsSuggestion = $derived(eventNoResultsSuggestion(records, filters));
	let noResultsClauses = $derived.by(() => {
		const update = noResultsSuggestion?.update ?? {};
		const constrains = (key: keyof EventBrowserFilters) =>
			Object.prototype.hasOwnProperty.call(update, key);
		const cameraName = cameras.find((camera) => camera.id === filters.cameraId)?.name;
		return [
			filters.cameraId
				? {
						label: `camera:${cameraName ?? filters.cameraId}`,
						constraining: constrains('cameraId')
					}
				: null,
			filters.type ? { label: `type:${filters.type}`, constraining: constrains('type') } : null,
			filters.source
				? { label: `source:${filters.source}`, constraining: constrains('source') }
				: null,
			filters.minimumConfidence === null
				? null
				: {
						label: `confidence:≥${filters.minimumConfidence}`,
						constraining: constrains('minimumConfidence')
					},
			filters.image === 'all'
				? null
				: { label: `image:${filters.image}`, constraining: constrains('image') },
			filters.query ? { label: `query:${filters.query}`, constraining: constrains('query') } : null,
			{ label: filters.date }
		].filter((clause): clause is { label: string; constraining?: boolean } => clause !== null);
	});

	onMount(() => {
		const requestedEventId = page.url.searchParams.get('event');
		const requestedEventCamera = page.url.searchParams.get('eventCamera');
		if (requestedEventId && requestedEventCamera) {
			selectedKey = `${encodeURIComponent(requestedEventCamera)}:${encodeURIComponent(requestedEventId)}`;
		}
		void initialize();
		const refreshTimer = window.setInterval(() => {
			currentDate = new Date().toISOString().slice(0, 10);
			if (!document.hidden) void refreshRecentEvents();
		}, LIVE_REFRESH_INTERVAL_MS);
		const handleVisibilityChange = () => {
			if (!document.hidden) void refreshRecentEvents();
		};
		document.addEventListener('visibilitychange', handleVisibilityChange);
		return () => {
			window.clearInterval(refreshTimer);
			document.removeEventListener('visibilitychange', handleVisibilityChange);
			loadController?.abort();
			refreshController?.abort();
			cancelPreviews();
			releaseRecordPreviews(records);
		};
	});

	async function initialize(): Promise<void> {
		try {
			cameras = await controlClient.getCameras();
			await loadEvents(filters.date);
		} catch (cause) {
			loadController?.abort();
			error = cause instanceof Error ? cause.message : 'Failed to load events';
		} finally {
			loading = false;
		}
	}

	async function refreshRecentEvents(): Promise<void> {
		if (!isToday || loading || loadingEarlier || refreshing || cameras.length === 0) return;
		const refreshDate = filters.date;
		const controller = new AbortController();
		refreshController = controller;
		refreshing = true;
		const day = eventBrowserDayBounds(filters.date);
		try {
			const result = await controlClient.queryStoredTimeline({
				sourceIds: cameras.map((camera) => camera.id),
				startMs: Math.max(day.startMs, day.endMs - EVENT_BROWSER_INITIAL_WINDOW_MS),
				endMs: day.endMs,
				includeEvents: true,
				includeAttachments: false,
				includeAvailability: false,
				signal: controller.signal,
				onPage: (page) => {
					if (controller.signal.aborted || filters.date !== refreshDate) return;
					records = mergeEventRecords(records, eventRecords(page.events));
				}
			});
			if (controller.signal.aborted || filters.date !== refreshDate) return;
			records = mergeEventRecords(records, eventRecords(result.events));
			refreshDelayed = false;
		} catch {
			if (!controller.signal.aborted) refreshDelayed = true;
		} finally {
			if (refreshController === controller) refreshController = null;
			refreshing = false;
		}
	}

	function refreshEvents(): void {
		if (isToday) void refreshRecentEvents();
		else void loadEvents(filters.date);
	}

	async function loadEvents(date: string): Promise<void> {
		loadController?.abort();
		refreshController?.abort();
		refreshController = null;
		refreshing = false;
		cancelPreviews();
		const controller = new AbortController();
		loadController = controller;
		const version = ++requestVersion;
		loading = true;
		error = null;
		releaseRecordPreviews(records);
		records = [];
		resultPage = 0;
		const day = eventBrowserDayBounds(date);
		dayStartMs = day.startMs;
		loadedStartMs = day.endMs;
		nextWindowDurationMs = EVENT_BROWSER_INITIAL_WINDOW_MS;
		try {
			do {
				await loadPreviousWindow(controller, version);
			} while (
				loadedStartMs > dayStartMs &&
				(records.length < EVENT_BROWSER_PAGE_SIZE ||
					(selectedKey !== null &&
						!records.some((record) => eventBrowserRecordKey(record) === selectedKey)))
			);
			if (version !== requestVersion) return;
			if (
				selectedKey !== null &&
				!records.some((record) => eventBrowserRecordKey(record) === selectedKey)
			) {
				selectedKey = null;
			}
			const selected = records.find((record) => eventBrowserRecordKey(record) === selectedKey);
			if (selected) requestEventPreview(selected);
			syncUrl();
		} catch (cause) {
			if (controller.signal.aborted || version !== requestVersion) return;
			error = cause instanceof Error ? cause.message : 'Failed to load events';
		} finally {
			if (version === requestVersion) loading = false;
		}
	}

	async function loadPreviousWindow(controller: AbortController, version: number): Promise<void> {
		const window = previousEventBrowserWindow(dayStartMs, loadedStartMs, nextWindowDurationMs);
		if (!window) return;
		const result = await controlClient.queryStoredTimeline({
			sourceIds: cameras.map((camera) => camera.id),
			startMs: window.startMs,
			endMs: window.endMs,
			includeEvents: true,
			includeAttachments: false,
			includeAvailability: false,
			signal: controller.signal,
			onPage: (page) => {
				if (version !== requestVersion) return;
				records = mergeEventRecords(records, eventRecords(page.events));
			}
		});
		if (version !== requestVersion) return;
		records = mergeEventRecords(records, eventRecords(result.events));
		loadedStartMs = window.startMs;
		nextWindowDurationMs = window.nextDurationMs;
	}

	async function showEarlierEvents(): Promise<void> {
		const nextPageStart = (resultPage + 1) * EVENT_BROWSER_PAGE_SIZE;
		if (nextPageStart < filteredRecords.length) {
			resultPage += 1;
			eventuallyEvictDistantPreviews();
			return;
		}
		if (loadedStartMs <= dayStartMs || !loadController || loadingEarlier) return;
		const controller = loadController;
		const version = requestVersion;
		loadingEarlier = true;
		try {
			const previousCount = filteredRecords.length;
			await loadPreviousWindow(controller, version);
			if (version !== requestVersion) return;
			if (filteredRecords.length > previousCount) {
				resultPage = Math.floor(previousCount / EVENT_BROWSER_PAGE_SIZE);
				eventuallyEvictDistantPreviews();
			}
		} catch (cause) {
			if (!controller.signal.aborted && version === requestVersion) {
				error = cause instanceof Error ? cause.message : 'Failed to load earlier events';
			}
		} finally {
			loadingEarlier = false;
		}
	}

	function showNewerEvents(): void {
		resultPage = Math.max(0, resultPage - 1);
		eventuallyEvictDistantPreviews();
	}

	function eventRecords(events: readonly RecordingEvent[]): EventBrowserRecord[] {
		const camerasById = new Map(cameras.map((camera) => [camera.id, camera]));
		return events.flatMap((event) => {
			const camera = event.source_id ? camerasById.get(event.source_id) : undefined;
			return camera ? [{ camera, event }] : [];
		});
	}

	function mergeEventRecords(
		current: readonly EventBrowserRecord[],
		incoming: readonly EventBrowserRecord[]
	): EventBrowserRecord[] {
		const merged = new Map(current.map((record) => [eventBrowserRecordKey(record), record]));
		for (const record of incoming) {
			const key = eventBrowserRecordKey(record);
			const existing = merged.get(key);
			merged.set(
				key,
				existing?.event.thumbnail_url && !record.event.thumbnail_url ? existing : record
			);
		}
		return [...merged.values()];
	}

	function requestEventPreview(record: EventBrowserRecord): void {
		const key = eventBrowserRecordKey(record);
		if (record.event.thumbnail_url || previewKeys.has(key)) return;
		if (!record.event.attachments?.some((attachment) => attachment.type === 'thumbnail')) return;
		previewKeys.add(key);
		setPreviewState(key, 'queued');
		previewQueue.push(record);
		startQueuedPreviews();
	}

	function startQueuedPreviews(): void {
		while (activePreviewCount < maxConcurrentPreviews) {
			const record = previewQueue.shift();
			if (!record) return;
			setPreviewState(eventBrowserRecordKey(record), 'loading');
			activePreviewCount += 1;
			void loadEventPreview(record).finally(() => {
				activePreviewCount -= 1;
				startQueuedPreviews();
			});
		}
	}

	async function loadEventPreview(record: EventBrowserRecord): Promise<void> {
		const key = eventBrowserRecordKey(record);
		const controller = new AbortController();
		previewControllers.set(key, controller);
		try {
			const result = await controlClient.queryStoredTimeline({
				sourceIds: [record.camera.id],
				startMs: record.event.start_time_ms,
				endMs: Math.max(
					record.event.end_time_ms ?? record.event.start_time_ms + 1,
					record.event.start_time_ms + 1
				),
				includeEvents: true,
				includeAttachments: true,
				includeAvailability: false,
				signal: controller.signal
			});
			const event = result.events.find((candidate) => candidate.id === record.event.id);
			if (!event?.thumbnail_url) {
				previewKeys.delete(key);
				setPreviewState(key, 'unavailable');
				return;
			}
			const shouldKeep =
				selectedKey === key ||
				visibleRecords.some((candidate) => eventBrowserRecordKey(candidate) === key);
			if (!shouldKeep) {
				releaseEventPreview(event);
				previewKeys.delete(key);
				return;
			}
			records = records.map((candidate) =>
				eventBrowserRecordKey(candidate) === key ? { ...candidate, event } : candidate
			);
		} catch (cause) {
			if (!controller.signal.aborted) {
				previewKeys.delete(key);
				setPreviewState(key, 'unavailable');
				console.warn('Failed to load event preview', cause);
			}
		} finally {
			if (previewControllers.get(key) === controller) previewControllers.delete(key);
		}
	}

	function cancelPreviews(retainedKeys: ReadonlySet<string> = new Set()): void {
		previewQueue = previewQueue.filter((record) => retainedKeys.has(eventBrowserRecordKey(record)));
		for (const key of previewKeys) {
			if (!retainedKeys.has(key)) previewKeys.delete(key);
		}
		for (const [key, controller] of previewControllers) {
			if (retainedKeys.has(key)) continue;
			controller.abort();
			previewControllers.delete(key);
		}
		previewStates = Object.fromEntries(
			Object.entries(previewStates).filter(([key]) => retainedKeys.has(key))
		);
	}

	function setPreviewState(key: string, state: EventPreviewState): void {
		previewStates = { ...previewStates, [key]: state };
	}

	function eventuallyEvictDistantPreviews(): void {
		queueMicrotask(evictDistantPreviews);
	}

	function evictDistantPreviews(): void {
		const retainedKeys = new Set(visibleRecords.map(eventBrowserRecordKey));
		if (selectedKey) retainedKeys.add(selectedKey);
		records = records.map((record) => {
			const key = eventBrowserRecordKey(record);
			if (retainedKeys.has(key) || !record.event.thumbnail_blob) return record;
			releaseEventPreview(record.event);
			previewKeys.delete(key);
			return {
				...record,
				event: { ...record.event, thumbnail_url: null, thumbnail_blob: undefined }
			};
		});
	}

	function releaseRecordPreviews(current: readonly EventBrowserRecord[]): void {
		for (const record of current) releaseEventPreview(record.event);
	}

	function releaseEventPreview(event: RecordingEvent): void {
		if (event.thumbnail_blob && event.thumbnail_url?.startsWith('blob:')) {
			URL.revokeObjectURL(event.thumbnail_url);
		}
	}

	function updateFilters(update: Partial<EventBrowserFilters>): void {
		const next = { ...filters, ...update };
		const dateChanged = next.date !== filters.date;
		const retainedPreviewKeys = new Set(
			filterEventBrowserRecords(records, next)
				.slice(0, EVENT_BROWSER_PAGE_SIZE)
				.map(eventBrowserRecordKey)
		);
		cancelPreviews(dateChanged ? undefined : retainedPreviewKeys);
		filters = next;
		resultPage = 0;
		eventuallyEvictDistantPreviews();
		selectedKey = null;
		focusedKey = null;
		if (dateChanged) void loadEvents(next.date);
		else syncUrl();
	}

	function selectRecord(record: EventBrowserRecord): void {
		selectedKey = eventBrowserRecordKey(record);
		requestEventPreview(record);
		syncUrl();
	}

	function closeDetail(): void {
		selectedKey = null;
		syncUrl();
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape' && selectedRecord !== null) closeDetail();
	}

	async function moveEventFocus(event: KeyboardEvent, record: EventBrowserRecord): Promise<void> {
		if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return;
		event.preventDefault();
		const currentIndex = visibleRecords.findIndex(
			(candidate) => eventBrowserRecordKey(candidate) === eventBrowserRecordKey(record)
		);
		const nextIndex = Math.max(
			0,
			Math.min(visibleRecords.length - 1, currentIndex + (event.key === 'ArrowDown' ? 1 : -1))
		);
		const next = visibleRecords[nextIndex];
		if (!next) return;
		focusedKey = eventBrowserRecordKey(next);
		await tick();
		document.querySelector<HTMLElement>(`[data-event-card="${CSS.escape(focusedKey)}"]`)?.focus();
	}

	function syncUrl(): void {
		const search = eventBrowserSearchParams(filters, selectedRecord);
		replaceState(`${resolve('/events')}?${search}`, {});
	}

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function clearFilters(): void {
		updateFilters({
			cameraId: null,
			type: null,
			source: null,
			minimumConfidence: null,
			image: 'all',
			query: ''
		});
	}
</script>

<svelte:head>
	<title>Events - KeepPeek</title>
</svelte:head>

<svelte:window onkeydown={handleKeydown} />

<div class="flex min-h-0 flex-col gap-3 px-4 py-3 md:p-4">
	<header class="flex min-h-10 flex-wrap items-center gap-3">
		<div>
			<h1 class="text-xl font-semibold">Events</h1>
			<p class="text-xs text-text-muted">Review detections across every camera.</p>
		</div>
		<div class="min-w-2 flex-1"></div>
		<div class="flex items-center gap-2">
			{#if isToday}
				<span
					class="inline-flex h-8 items-center gap-2 px-1.5 font-mono text-2xs tracking-caps text-text-muted"
				>
					<span class="size-1.5 rounded-full {refreshDelayed ? 'bg-destructive' : 'bg-primary'}"
					></span>
					{refreshDelayed ? 'UPDATE DELAYED' : refreshing ? 'UPDATING' : 'LIVE'}
				</span>
			{/if}
			<button
				type="button"
				class="grid size-8 place-items-center rounded-sm text-text-muted hover:bg-raised hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-45"
				title="Refresh events"
				aria-label="Refresh events"
				disabled={loading || refreshing}
				onclick={refreshEvents}
			>
				<RefreshCwIcon class="size-3.5 {refreshing ? 'animate-spin' : ''}" />
			</button>
		</div>
	</header>

	<section
		class="space-y-2 rounded-md border border-hairline bg-surface p-3"
		aria-label="Event filters"
	>
		<div class="grid gap-2 md:grid-cols-[minmax(13rem,1fr)_10rem_10rem_9rem_9rem]">
			<label class="relative block">
				<SearchIcon
					class="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-text-faint"
				/>
				<span class="sr-only">Search events</span>
				<input
					type="search"
					value={filters.query}
					placeholder="Camera, type, source, zone…"
					class="h-9 w-full rounded-sm border border-hairline bg-raised pr-3 pl-8 text-xs outline-none placeholder:text-text-faint focus:border-ring focus:ring-1 focus:ring-ring"
					oninput={(event) => updateFilters({ query: event.currentTarget.value })}
				/>
			</label>
			<label class="grid gap-1 font-mono text-2xs tracking-caps text-text-faint">
				<span class="sr-only">Event date</span>
				<input
					type="date"
					value={filters.date}
					class="h-9 rounded-sm border border-hairline bg-raised px-2 text-xs tracking-normal text-foreground outline-none focus:border-ring focus:ring-1 focus:ring-ring"
					onchange={(event) => updateFilters({ date: event.currentTarget.value })}
				/>
			</label>
			<label>
				<span class="sr-only">Camera filter</span>
				<select
					value={filters.cameraId ?? ''}
					class="h-9 w-full rounded-sm border border-hairline bg-raised px-2 text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
					onchange={(event) => updateFilters({ cameraId: event.currentTarget.value || null })}
				>
					<option value="">All cameras</option>
					{#each cameras as camera (camera.id)}
						<option value={camera.id}>{cameraLabel(camera)}</option>
					{/each}
				</select>
			</label>
			<label>
				<span class="sr-only">Event type filter</span>
				<select
					value={filters.type ?? ''}
					class="h-9 w-full rounded-sm border border-hairline bg-raised px-2 text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
					onchange={(event) => updateFilters({ type: event.currentTarget.value || null })}
				>
					{#each eventKinds as kind (kind)}
						<option value={kind === 'all' ? '' : kind}>
							{kind === 'all' ? 'All types' : kind.charAt(0).toUpperCase() + kind.slice(1)}
						</option>
					{/each}
					{#each availableTypes.filter((kind) => !eventKinds.includes(kind as (typeof eventKinds)[number])) as kind (kind)}
						<option value={kind}>{kind}</option>
					{/each}
				</select>
			</label>
			<label>
				<span class="sr-only">Image filter</span>
				<select
					value={filters.image}
					class="h-9 w-full rounded-sm border border-hairline bg-raised px-2 text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
					onchange={(event) =>
						updateFilters({ image: event.currentTarget.value as EventImageFilter })}
				>
					{#each imageFilters as filter (filter.value)}
						<option value={filter.value}>{filter.label}</option>
					{/each}
				</select>
			</label>
		</div>
		<div class="flex flex-wrap items-center gap-2">
			<SlidersHorizontalIcon class="size-3.5 text-text-faint" />
			<label class="flex items-center gap-2 text-xs text-text-muted">
				Minimum confidence
				<input
					type="number"
					min="0"
					max="1"
					step="0.05"
					value={filters.minimumConfidence ?? ''}
					placeholder="Any"
					class="h-8 w-20 rounded-sm border border-hairline bg-raised px-2 font-mono text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
					onchange={(event) => {
						const value = event.currentTarget.valueAsNumber;
						updateFilters({ minimumConfidence: Number.isFinite(value) ? value : null });
					}}
				/>
			</label>
			<label class="flex items-center gap-2 text-xs text-text-muted">
				Reported by
				<select
					value={filters.source ?? ''}
					class="h-8 rounded-sm border border-hairline bg-raised px-2 text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
					onchange={(event) =>
						updateFilters({
							source: (event.currentTarget.value as RecordingEvent['source']) || null
						})}
				>
					<option value="">Any source</option>
					<option value="camera">Camera source</option>
					<option value="keeppeek">KeepPeek pipeline</option>
				</select>
			</label>
			<button
				type="button"
				class="ml-auto h-8 rounded-sm px-2.5 text-xs text-text-muted hover:bg-raised hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				onclick={clearFilters}
			>
				Clear filters
			</button>
		</div>
	</section>

	<div class="flex items-center gap-2 text-xs text-text-muted" role="status" aria-live="polite">
		<span
			>{loading && records.length === 0
				? 'Loading events'
				: `${filteredRecords.length} ${filteredRecords.length === 1 ? 'event' : 'events'}`}</span
		>
		<span aria-hidden="true">·</span>
		<span>{eventFilterSummary(filters)}</span>
	</div>

	{#if error}
		<div
			class="border-y border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
			role="alert"
		>
			{error}
		</div>
	{:else if loading && records.length === 0}
		<div class="grid min-h-64 place-items-center border-y border-hairline text-sm text-text-muted">
			Loading events…
		</div>
	{:else if filteredRecords.length === 0}
		<div class="space-y-3">
			<EventNoResultsState
				clauses={noResultsClauses}
				title={loadedStartMs > dayStartMs
					? 'No matching events in the loaded window.'
					: 'No events found.'}
				description={loadedStartMs > dayStartMs
					? `No loaded events match ${eventFilterSummary(filters)}.`
					: `No events found for ${eventFilterSummary(filters)}.`}
				suggestionLabel={noResultsSuggestion?.label}
				onloosen={noResultsSuggestion
					? () => updateFilters(noResultsSuggestion!.update)
					: undefined}
				onclear={clearFilters}
				class="min-h-64 rounded-md border border-dashed border-hairline-strong"
			/>
			{#if loadedStartMs > dayStartMs}
				<div class="flex justify-center">
					<button
						type="button"
						class="h-9 rounded-sm border border-hairline px-3 text-xs disabled:opacity-40"
						disabled={loadingEarlier}
						onclick={() => void showEarlierEvents()}
					>
						{loadingEarlier ? 'Searching earlier...' : 'Search earlier events'}
					</button>
				</div>
			{/if}
		</div>
	{:else}
		<div
			class="grid grid-cols-[repeat(auto-fill,minmax(min(100%,14rem),1fr))] gap-3"
			aria-label="Event results"
		>
			{#each visibleRecords as record, index (eventBrowserRecordKey(record))}
				<EventResultCard
					{record}
					previewState={previewStates[eventBrowserRecordKey(record)] ?? 'idle'}
					selected={selectedKey === eventBrowserRecordKey(record)}
					mobileVariant={index === 0 ? 'hero' : 'row'}
					tabindex={focusedKey === eventBrowserRecordKey(record) ||
					(focusedKey === null && index === 0)
						? 0
						: -1}
					onfocus={() => (focusedKey = eventBrowserRecordKey(record))}
					onkeydown={(event) => void moveEventFocus(event, record)}
					onclick={() => selectRecord(record)}
					onpreviewrequest={() => requestEventPreview(record)}
				/>
			{/each}
		</div>
		{#if filteredRecords.length > EVENT_BROWSER_PAGE_SIZE || loadedStartMs > dayStartMs}
			<nav class="flex items-center justify-center gap-3 py-2" aria-label="Event result pages">
				<button
					type="button"
					class="h-9 rounded-sm border border-hairline px-3 text-xs disabled:opacity-40"
					disabled={resultPage === 0}
					onclick={showNewerEvents}
				>
					Newer events
				</button>
				<span class="font-mono text-2xs text-text-faint">
					{visibleResultStart}-{visibleResultEnd} of {filteredRecords.length} loaded
				</span>
				<button
					type="button"
					class="h-9 rounded-sm border border-hairline px-3 text-xs disabled:opacity-40"
					disabled={loadingEarlier ||
						((resultPage + 1) * EVENT_BROWSER_PAGE_SIZE >= filteredRecords.length &&
							loadedStartMs <= dayStartMs)}
					onclick={() => void showEarlierEvents()}
				>
					{loadingEarlier ? 'Loading earlier...' : 'Earlier events'}
				</button>
			</nav>
		{/if}
	{/if}
</div>

{#if selectedRecord}
	<button
		type="button"
		class="fixed inset-0 z-[80] cursor-default bg-black/55"
		aria-label="Close event detail backdrop"
		onclick={closeDetail}
	></button>
	<EventDetailDrawer
		record={selectedRecord}
		previewState={previewStates[eventBrowserRecordKey(selectedRecord)] ?? 'idle'}
		onclose={closeDetail}
		onpreviewretry={() => requestEventPreview(selectedRecord)}
	/>
{/if}
