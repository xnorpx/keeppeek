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
		eventBrowserRecordKey,
		eventBrowserSearchParams,
		eventFilterSummary,
		eventNoResultsSuggestion,
		filterEventBrowserRecords,
		parseEventBrowserFilters,
		type EventBrowserFilters,
		type EventBrowserRecord,
		type EventImageFilter
	} from '$lib/event-browser';
	import type { CameraListItem, RecordingEvent } from '$lib/types';
	import SearchIcon from '@lucide/svelte/icons/search';
	import SlidersHorizontalIcon from '@lucide/svelte/icons/sliders-horizontal';

	const today = new Date().toISOString().slice(0, 10);
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
	let error = $state<string | null>(null);
	let requestVersion = 0;
	let filteredRecords = $derived(filterEventBrowserRecords(records, filters));
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
	});

	async function initialize(): Promise<void> {
		try {
			cameras = await controlClient.getCameras();
			await loadEvents(filters.date);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to load events';
		} finally {
			loading = false;
		}
	}

	async function loadEvents(date: string): Promise<void> {
		const version = ++requestVersion;
		loading = true;
		error = null;
		const results = await Promise.all(
			cameras.map(async (camera): Promise<EventBrowserRecord[]> => {
				try {
					const response = await controlClient.getRecordingEvents(camera.id, date);
					return response.events.map((event) => ({ camera, event }));
				} catch {
					return [];
				}
			})
		);
		if (version !== requestVersion) return;
		records = results.flat();
		if (
			selectedKey !== null &&
			!records.some((record) => eventBrowserRecordKey(record) === selectedKey)
		) {
			selectedKey = null;
		}
		loading = false;
		syncUrl();
	}

	function updateFilters(update: Partial<EventBrowserFilters>): void {
		const next = { ...filters, ...update };
		const dateChanged = next.date !== filters.date;
		filters = next;
		selectedKey = null;
		focusedKey = null;
		if (dateChanged) void loadEvents(next.date);
		else syncUrl();
	}

	function selectRecord(record: EventBrowserRecord): void {
		selectedKey = eventBrowserRecordKey(record);
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
		const currentIndex = filteredRecords.findIndex(
			(candidate) => eventBrowserRecordKey(candidate) === eventBrowserRecordKey(record)
		);
		const nextIndex = Math.max(
			0,
			Math.min(filteredRecords.length - 1, currentIndex + (event.key === 'ArrowDown' ? 1 : -1))
		);
		const next = filteredRecords[nextIndex];
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
			<p class="text-xs text-text-muted">Everything another source reported.</p>
		</div>
		<div class="min-w-2 flex-1"></div>
		<span class="font-mono text-2xs tracking-caps text-text-faint">URL CARRIES THIS QUERY</span>
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

	<div class="flex items-center gap-2 text-xs text-text-muted" role="status">
		<span>{filteredRecords.length} {filteredRecords.length === 1 ? 'event' : 'events'}</span>
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
		<EventNoResultsState
			clauses={noResultsClauses}
			title="No events found."
			description={`No events found for ${eventFilterSummary(filters)}.`}
			suggestionLabel={noResultsSuggestion?.label}
			onloosen={noResultsSuggestion ? () => updateFilters(noResultsSuggestion!.update) : undefined}
			onclear={clearFilters}
			class="min-h-64 rounded-md border border-dashed border-hairline-strong"
		/>
	{:else}
		<div
			class="grid grid-cols-[repeat(auto-fill,minmax(min(100%,14rem),1fr))] gap-3"
			aria-label="Event results"
		>
			{#each filteredRecords as record, index (eventBrowserRecordKey(record))}
				<EventResultCard
					{record}
					selected={selectedKey === eventBrowserRecordKey(record)}
					mobileVariant={index === 0 ? 'hero' : 'row'}
					tabindex={focusedKey === eventBrowserRecordKey(record) ||
					(focusedKey === null && index === 0)
						? 0
						: -1}
					onfocus={() => (focusedKey = eventBrowserRecordKey(record))}
					onkeydown={(event) => void moveEventFocus(event, record)}
					onclick={() => selectRecord(record)}
				/>
			{/each}
		</div>
	{/if}
</div>

{#if selectedRecord}
	<button
		type="button"
		class="fixed inset-0 z-[80] cursor-default bg-black/55"
		aria-label="Close event detail backdrop"
		onclick={closeDetail}
	></button>
	<EventDetailDrawer record={selectedRecord} onclose={closeDetail} />
{/if}
