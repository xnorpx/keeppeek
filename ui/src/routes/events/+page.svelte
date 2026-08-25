<script lang="ts">
	import { pushState, replaceState } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount, tick } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import { decodeEventKeyframePreview } from '$lib/event-keyframe-preview';
	import EventDetailDrawer from '$lib/components/EventDetailDrawer.svelte';
	import EventNoResultsState from '$lib/components/EventNoResultsState.svelte';
	import EventResultCard from '$lib/components/EventResultCard.svelte';
	import {
		EVENT_BROWSER_PAGE_SIZE,
		eventBrowserQueryBounds,
		eventBrowserRecordKey,
		eventBrowserSearchParams,
		eventFilterSummary,
		parseEventBrowserFilters,
		type EventBrowserFilters,
		type EventBrowserRecord,
		type EventImageFilter,
		type EventPreviewState
	} from '$lib/event-browser';
	import type { EventPreviewHit, EventPreviewPage } from '$lib/control-client';
	import type { CameraListItem, RecordingEvent } from '$lib/types';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SearchIcon from '@lucide/svelte/icons/search';
	import SlidersHorizontalIcon from '@lucide/svelte/icons/sliders-horizontal';

	const today = new Date().toISOString().slice(0, 10);
	const LIVE_REFRESH_INTERVAL_MS = 5_000;
	const SEARCH_DEBOUNCE_MS = 250;
	const MAX_EVENT_KEYFRAME_BYTES = 4 * 1_048_576;
	const MAX_RETAINED_PAGE_TOKENS = 32;
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
	let pageTokens = $state.raw<string[]>(['']);
	let pageTokenBase = $state(0);
	let pageIndex = $state(0);
	let nextPageToken = $state('');
	let loadingEarlier = $state(false);
	let searchTimer: number | undefined;
	let selectedIdentity = $state<{ eventId: string; cameraId: string } | null>(null);
	let selectedDetachedRecord = $state.raw<EventBrowserRecord | null>(null);
	let selectionError = $state<string | null>(null);
	let recoveryNotice = $state<string | null>(null);
	type EventPageState = {
		eventPageToken?: string;
		eventPageTokens?: string[];
		eventPageTokenBase?: number;
		eventPageIndex?: number;
		eventScrollY?: number;
	};
	let noResultsSuggestion = $state<{
		label: string;
		update: Partial<EventBrowserFilters>;
	} | null>(null);
	let isToday = $derived(filters.date === currentDate);
	let visibleRecords = $derived(records);
	let visibleResultStart = $derived(
		records.length === 0 ? 0 : pageIndex * EVENT_BROWSER_PAGE_SIZE + 1
	);
	let visibleResultEnd = $derived(pageIndex * EVENT_BROWSER_PAGE_SIZE + records.length);
	let selectedRecord = $derived(
		selectedKey === null
			? null
			: (records.find((record) => eventBrowserRecordKey(record) === selectedKey) ??
					(selectedDetachedRecord && eventBrowserRecordKey(selectedDetachedRecord) === selectedKey
						? selectedDetachedRecord
						: null))
	);
	let availableTypes = $derived(
		[...new Set(records.map((record) => record.event.kind.toLocaleLowerCase()))].toSorted()
	);
	let noResultsClauses = $derived.by(() => {
		const update = noResultsSuggestion?.update ?? {};
		const constrains = (key: keyof EventBrowserFilters) =>
			Object.prototype.hasOwnProperty.call(update, key);
		const cameraName = cameras.find((camera) => camera.id === filters.cameraId)?.name;
		return [
			filters.startTime
				? { label: `from:${filters.startTime} UTC`, constraining: constrains('startTime') }
				: null,
			filters.endTime
				? { label: `to:${filters.endTime} UTC`, constraining: constrains('endTime') }
				: null,
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
			filters.zone ? { label: `zone:${filters.zone}`, constraining: constrains('zone') } : null,
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
		restoreSelection(new URL(window.location.href).searchParams);
		void initialize();
		const refreshTimer = window.setInterval(() => {
			currentDate = new Date().toISOString().slice(0, 10);
			if (!document.hidden) void refreshRecentEvents();
		}, LIVE_REFRESH_INTERVAL_MS);
		const handleVisibilityChange = () => {
			if (!document.hidden) void refreshRecentEvents();
		};
		const handlePopState = () => {
			const url = new URL(window.location.href);
			const nextFilters = parseEventBrowserFilters(url.searchParams, currentDate);
			const filtersChanged =
				eventBrowserSearchParams(nextFilters).toString() !==
				eventBrowserSearchParams(filters).toString();
			filters = nextFilters;
			restoreSelection(url.searchParams);
			if (!filtersChanged) return;
			const state = history.state as EventPageState | null;
			pageTokens = state?.eventPageTokens ?? [''];
			pageTokenBase = Number.isInteger(state?.eventPageTokenBase)
				? Math.max(0, state!.eventPageTokenBase!)
				: 0;
			const token = state?.eventPageToken ?? '';
			const index = Number.isInteger(state?.eventPageIndex) ? state!.eventPageIndex! : 0;
			void loadPage(token, Math.max(0, index)).then(() => {
				if (typeof state?.eventScrollY === 'number') window.scrollTo({ top: state.eventScrollY });
			});
		};
		document.addEventListener('visibilitychange', handleVisibilityChange);
		window.addEventListener('popstate', handlePopState);
		return () => {
			window.clearInterval(refreshTimer);
			if (searchTimer !== undefined) window.clearTimeout(searchTimer);
			document.removeEventListener('visibilitychange', handleVisibilityChange);
			window.removeEventListener('popstate', handlePopState);
			loadController?.abort();
			refreshController?.abort();
			cancelPreviews();
			const detachedRecords = records;
			const detachedSelection = selectedDetachedRecord;
			queueMicrotask(() => {
				releaseRecordPreviews(detachedRecords);
				if (detachedSelection) releaseEventPreview(detachedSelection);
			});
		};
	});

	async function initialize(): Promise<void> {
		try {
			cameras = await controlClient.getCameras();
			const state = page.state as EventPageState;
			const restoredTokens = state.eventPageTokens?.filter(
				(token): token is string => typeof token === 'string'
			);
			const restoredIndex = Number.isInteger(state.eventPageIndex)
				? Math.max(0, state.eventPageIndex ?? 0)
				: 0;
			const restoredBase = Number.isInteger(state.eventPageTokenBase)
				? Math.max(0, state.eventPageTokenBase ?? 0)
				: 0;
			if (restoredTokens?.[restoredIndex - restoredBase] !== undefined) {
				pageTokens = restoredTokens;
				pageTokenBase = restoredBase;
				await loadPage(restoredTokens[restoredIndex - restoredBase]!, restoredIndex);
			} else {
				await loadFirstPage();
			}
			syncUrl();
			if (typeof state.eventScrollY === 'number') {
				await tick();
				window.scrollTo({ top: state.eventScrollY });
			}
		} catch (cause) {
			loadController?.abort();
			error = cause instanceof Error ? cause.message : 'Failed to load events';
		} finally {
			loading = false;
		}
	}

	async function refreshRecentEvents(): Promise<void> {
		if (
			!isToday ||
			pageIndex !== 0 ||
			loading ||
			loadingEarlier ||
			refreshing ||
			cameras.length === 0
		) {
			return;
		}
		const refreshVersion = requestVersion;
		const controller = new AbortController();
		refreshController = controller;
		refreshing = true;
		try {
			const result = await queryEventPage(filters, '', controller.signal);
			if (controller.signal.aborted || refreshVersion !== requestVersion) return;
			const incoming = eventRecords(result.hits);
			const previous = records;
			records = mergeEventRecords(previous, incoming);
			nextPageToken = result.nextPageToken;
			await tick();
			for (const record of previous) {
				if (
					!records.some(
						(candidate) => eventBrowserRecordKey(candidate) === eventBrowserRecordKey(record)
					)
				) {
					releaseEventPreview(record);
				}
			}
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
		else void loadPage(pageTokens[pageIndex - pageTokenBase] ?? '', pageIndex);
	}

	async function loadFirstPage(): Promise<void> {
		pageTokens = [''];
		pageTokenBase = 0;
		pageIndex = 0;
		nextPageToken = '';
		await loadPage('', 0);
	}

	async function loadPage(token: string, targetPage: number, recoverToken = true): Promise<void> {
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
		selectionError = null;
		noResultsSuggestion = null;
		const detachedRecords = records;
		const detachedSelection = selectedDetachedRecord;
		records = [];
		selectedDetachedRecord = null;
		await tick();
		releaseRecordPreviews(detachedRecords);
		if (detachedSelection) releaseEventPreview(detachedSelection);
		try {
			const result = await queryEventPage(filters, token, controller.signal);
			if (controller.signal.aborted || version !== requestVersion) return;
			records = eventRecords(result.hits);
			pageIndex = targetPage;
			nextPageToken = result.nextPageToken;
			const relativePage = targetPage - pageTokenBase;
			pageTokens = [...pageTokens.slice(0, relativePage), token];
			if (pageTokens.length > MAX_RETAINED_PAGE_TOKENS) {
				const removed = pageTokens.length - MAX_RETAINED_PAGE_TOKENS;
				pageTokens = pageTokens.slice(removed);
				pageTokenBase += removed;
			}
			let selected = records.find((record) => eventBrowserRecordKey(record) === selectedKey);
			if (!selected && selectedIdentity) {
				const selectedPage = await queryEventPage(filters, '', controller.signal, 1, [
					selectedIdentity.eventId
				]);
				if (controller.signal.aborted || version !== requestVersion) return;
				selectedDetachedRecord =
					eventRecords(selectedPage.hits).find(
						(record) => eventBrowserRecordKey(record) === selectedKey
					) ?? null;
				selected = selectedDetachedRecord ?? undefined;
			}
			if (selected) requestEventPreview(selected);
			else if (selectedIdentity)
				selectionError = 'The selected event is not available on this page.';
			if (records.length === 0) {
				loading = false;
				await findNoResultsSuggestion(version, controller.signal);
			}
		} catch (cause) {
			if (controller.signal.aborted || version !== requestVersion) return;
			const message = cause instanceof Error ? cause.message : 'Failed to load events';
			if (token && recoverToken && /page token|snapshot changed/i.test(message)) {
				recoveryNotice = 'Events changed while you were browsing. Refreshed from the newest page.';
				await loadFirstPage();
				return;
			}
			error = message;
		} finally {
			if (version === requestVersion) {
				loading = false;
				if (loadController === controller) loadController = null;
			}
		}
	}

	function queryEventPage(
		queryFilters: EventBrowserFilters,
		pageToken: string,
		signal: AbortSignal,
		pageSize = EVENT_BROWSER_PAGE_SIZE,
		eventIds: readonly string[] = []
	): Promise<EventPreviewPage> {
		const bounds = eventBrowserQueryBounds(queryFilters);
		return controlClient.searchEventMetadata({
			eventIds,
			sourceIds: queryFilters.cameraId
				? [queryFilters.cameraId]
				: cameras.map((camera) => camera.id),
			streamId: 'main',
			startMs: bounds.startMs,
			endMs: bounds.endMs,
			eventTypes: queryFilters.type ? [queryFilters.type] : [],
			origins: queryFilters.source ? [queryFilters.source] : [],
			zones: queryFilters.zone ? [queryFilters.zone] : [],
			minimumConfidence: queryFilters.minimumConfidence ?? undefined,
			image: queryFilters.image,
			text: queryFilters.query || undefined,
			pageSize,
			pageToken: pageToken || undefined,
			signal
		});
	}

	async function showEarlierEvents(): Promise<void> {
		if (!nextPageToken || loadingEarlier) return;
		loadingEarlier = true;
		try {
			const targetPage = pageIndex + 1;
			const token = pageTokens[targetPage - pageTokenBase] ?? nextPageToken;
			await loadPage(token, targetPage);
			syncUrl();
		} finally {
			loadingEarlier = false;
		}
	}

	function showNewerEvents(): void {
		if (pageIndex <= pageTokenBase || loading) return;
		void loadPage(pageTokens[pageIndex - 1 - pageTokenBase] ?? '', pageIndex - 1).then(() =>
			syncUrl()
		);
	}

	function eventRecords(hits: readonly EventPreviewHit[]): EventBrowserRecord[] {
		const camerasById = new Map(cameras.map((camera) => [camera.id, camera]));
		return hits.flatMap((hit) => {
			const camera = camerasById.get(hit.sourceId);
			if (!camera) return [];
			const previewKeyframe = hit.keyframes
				.filter((keyframe) => keyframe.byteLength <= MAX_EVENT_KEYFRAME_BYTES)
				.toSorted(
					(left, right) =>
						Math.abs(left.eventTimeMs - hit.startMs) - Math.abs(right.eventTimeMs - hit.startMs)
				)[0];
			const hasPreview = hit.hasImageAttachment || previewKeyframe !== undefined;
			const event: RecordingEvent = {
				id: hit.eventId,
				source_id: hit.sourceId,
				source: hit.origin,
				kind: hit.eventType,
				start_time_ms: hit.startMs,
				end_time_ms: hit.endMs,
				confidence: hit.confidence,
				bbox: hit.bbox,
				zone: hit.zone,
				text: hit.text,
				thumbnail_url: null,
				attachments: hasPreview
					? [
							{
								id: 'canonical-preview',
								type: 'thumbnail',
								content_type: 'image/jpeg',
								byte_length: previewKeyframe?.byteLength ?? null,
								ordinal: 0,
								timestamp_ms: previewKeyframe?.eventTimeMs ?? hit.startMs
							}
						]
					: []
			};
			return [{ camera, event, previewKeyframe }];
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
				existing?.previewObjectUrl
					? {
							...record,
							event: { ...record.event, thumbnail_url: existing.event.thumbnail_url },
							previewObjectUrl: true
						}
					: record
			);
		}
		return incoming.map((record) => merged.get(eventBrowserRecordKey(record)) ?? record);
	}

	function requestEventPreview(record: EventBrowserRecord): void {
		const key = eventBrowserRecordKey(record);
		if (record.event.thumbnail_url || previewKeys.has(key)) return;
		if (!record.previewKeyframe) {
			if (record.event.attachments?.some((attachment) => attachment.type === 'thumbnail')) {
				setPreviewState(key, 'unavailable');
			}
			return;
		}
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
			const media = await controlClient.fetchEventPreviewKeyframe(
				record.previewKeyframe!,
				controller.signal
			);
			const url = await decodeEventKeyframePreview(media);
			const shouldKeep =
				selectedKey === key ||
				visibleRecords.some((candidate) => eventBrowserRecordKey(candidate) === key);
			if (!shouldKeep) {
				URL.revokeObjectURL(url);
				previewKeys.delete(key);
				return;
			}
			records = records.map((candidate) =>
				eventBrowserRecordKey(candidate) === key
					? {
							...candidate,
							event: { ...candidate.event, thumbnail_url: url },
							previewObjectUrl: true
						}
					: candidate
			);
			if (selectedDetachedRecord && eventBrowserRecordKey(selectedDetachedRecord) === key) {
				selectedDetachedRecord = {
					...selectedDetachedRecord,
					event: { ...selectedDetachedRecord.event, thumbnail_url: url },
					previewObjectUrl: true
				};
			}
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

	function releaseRecordPreviews(current: readonly EventBrowserRecord[]): void {
		for (const record of current) releaseEventPreview(record);
	}

	function releaseEventPreview(record: EventBrowserRecord): void {
		if (record.previewObjectUrl && record.event.thumbnail_url?.startsWith('blob:')) {
			URL.revokeObjectURL(record.event.thumbnail_url);
		}
	}

	function updateFilters(update: Partial<EventBrowserFilters>, debounce = false): void {
		const next = { ...filters, ...update };
		loadController?.abort();
		refreshController?.abort();
		requestVersion += 1;
		cancelPreviews();
		const detachedRecords = records;
		const detachedSelection = selectedDetachedRecord;
		records = [];
		selectedDetachedRecord = null;
		void tick().then(() => {
			releaseRecordPreviews(detachedRecords);
			if (detachedSelection) releaseEventPreview(detachedSelection);
		});
		filters = next;
		selectedKey = null;
		selectedIdentity = null;
		selectionError = null;
		focusedKey = null;
		pageTokens = [''];
		pageTokenBase = 0;
		pageIndex = 0;
		nextPageToken = '';
		loading = true;
		syncUrl();
		if (searchTimer !== undefined) window.clearTimeout(searchTimer);
		if (debounce) {
			searchTimer = window.setTimeout(() => {
				searchTimer = undefined;
				void loadFirstPage();
			}, SEARCH_DEBOUNCE_MS);
		} else {
			void loadFirstPage();
		}
	}

	function selectRecord(record: EventBrowserRecord): void {
		selectedKey = eventBrowserRecordKey(record);
		selectedIdentity = { eventId: record.event.id, cameraId: record.camera.id };
		selectionError = null;
		requestEventPreview(record);
		syncUrl('push');
	}

	function closeDetail(): void {
		const detachedSelection = selectedDetachedRecord;
		selectedKey = null;
		selectedIdentity = null;
		selectedDetachedRecord = null;
		selectionError = null;
		syncUrl();
		if (detachedSelection) void tick().then(() => releaseEventPreview(detachedSelection));
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

	function syncUrl(mode: 'push' | 'replace' = 'replace'): void {
		const search = eventBrowserSearchParams(filters, selectedRecord);
		if (!selectedRecord && selectedIdentity) {
			search.set('event', selectedIdentity.eventId);
			search.set('eventCamera', selectedIdentity.cameraId);
		}
		const state: EventPageState = {
			eventPageToken: pageTokens[pageIndex - pageTokenBase] ?? '',
			eventPageTokens: pageTokens,
			eventPageTokenBase: pageTokenBase,
			eventPageIndex: pageIndex,
			eventScrollY: window.scrollY
		};
		const url = `${resolve('/events')}?${search}`;
		if (mode === 'push') pushState(url, state);
		else replaceState(url, state);
	}

	function restoreSelection(params: URLSearchParams): void {
		const eventId = params.get('event');
		const cameraId = params.get('eventCamera');
		selectedIdentity = eventId && cameraId ? { eventId, cameraId } : null;
		selectedKey = selectedIdentity
			? `${encodeURIComponent(selectedIdentity.cameraId)}:${encodeURIComponent(selectedIdentity.eventId)}`
			: null;
	}

	async function findNoResultsSuggestion(version: number, signal: AbortSignal): Promise<void> {
		const candidates: Array<{
			label: string;
			update: Partial<EventBrowserFilters>;
		}> = [
			...(filters.query ? [{ label: `Clear “${filters.query}”`, update: { query: '' } }] : []),
			...(filters.minimumConfidence !== null
				? [{ label: 'Remove confidence limit', update: { minimumConfidence: null } }]
				: []),
			...(filters.cameraId ? [{ label: 'Any camera', update: { cameraId: null } }] : []),
			...(filters.type ? [{ label: 'Any event type', update: { type: null } }] : []),
			...(filters.source ? [{ label: 'Any source', update: { source: null } }] : []),
			...(filters.zone ? [{ label: 'Any zone', update: { zone: null } }] : []),
			...(filters.image !== 'all'
				? [{ label: 'Any image state', update: { image: 'all' as const } }]
				: []),
			...(filters.startTime || filters.endTime
				? [{ label: 'Use the full day', update: { startTime: null, endTime: null } }]
				: [])
		];
		for (const candidate of candidates) {
			try {
				const result = await queryEventPage({ ...filters, ...candidate.update }, '', signal);
				if (signal.aborted || version !== requestVersion) return;
				if (result.hits.length === 0) continue;
				const count = result.nextPageToken ? '' : ` · ${result.hits.length} results`;
				noResultsSuggestion = {
					label: `${candidate.label}${count}`,
					update: candidate.update
				};
				return;
			} catch {
				if (signal.aborted || version !== requestVersion) return;
			}
		}
	}

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ?? camera.id;
	}

	function clearFilters(): void {
		updateFilters({
			startTime: null,
			endTime: null,
			cameraId: null,
			type: null,
			source: null,
			zone: null,
			minimumConfidence: null,
			image: 'all',
			query: ''
		});
	}

	function updateStartTime(value: string): void {
		const startTime = value || null;
		updateFilters({
			startTime,
			...(startTime && filters.endTime && startTime >= filters.endTime ? { endTime: null } : {})
		});
	}

	function updateEndTime(value: string): void {
		const endTime = value || null;
		updateFilters({
			endTime,
			...(endTime && filters.startTime && endTime <= filters.startTime ? { startTime: null } : {})
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
		<div
			class="grid gap-2 md:grid-cols-2 xl:grid-cols-[minmax(13rem,1fr)_9.5rem_8rem_8rem_10rem_9rem_9rem]"
		>
			<label class="relative block md:col-span-2 xl:col-span-1">
				<SearchIcon
					class="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-text-faint"
				/>
				<span class="sr-only">Search events</span>
				<input
					type="search"
					value={filters.query}
					placeholder="Search indexed event text…"
					class="h-9 w-full rounded-sm border border-hairline bg-raised pr-3 pl-8 text-xs outline-none placeholder:text-text-faint focus:border-ring focus:ring-1 focus:ring-ring"
					oninput={(event) => updateFilters({ query: event.currentTarget.value }, true)}
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
			<label class="grid gap-1 font-mono text-2xs tracking-caps text-text-faint">
				<span class="sr-only">Start time UTC</span>
				<input
					type="time"
					value={filters.startTime ?? ''}
					class="h-9 rounded-sm border border-hairline bg-raised px-2 text-xs tracking-normal text-foreground outline-none focus:border-ring focus:ring-1 focus:ring-ring"
					onchange={(event) => updateStartTime(event.currentTarget.value)}
				/>
			</label>
			<label class="grid gap-1 font-mono text-2xs tracking-caps text-text-faint">
				<span class="sr-only">End time UTC</span>
				<input
					type="time"
					value={filters.endTime ?? ''}
					class="h-9 rounded-sm border border-hairline bg-raised px-2 text-xs tracking-normal text-foreground outline-none focus:border-ring focus:ring-1 focus:ring-ring"
					onchange={(event) => updateEndTime(event.currentTarget.value)}
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
					{#if filters.type && !eventKinds.includes(filters.type as (typeof eventKinds)[number]) && !availableTypes.includes(filters.type)}
						<option value={filters.type}>{filters.type}</option>
					{/if}
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
				Zone
				<input
					type="text"
					value={filters.zone ?? ''}
					placeholder="Any"
					class="h-8 w-28 rounded-sm border border-hairline bg-raised px-2 text-xs outline-none placeholder:text-text-faint focus:border-ring focus:ring-1 focus:ring-ring"
					oninput={(event) => updateFilters({ zone: event.currentTarget.value || null }, true)}
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
				? loadingEarlier
					? 'Loading earlier events'
					: 'Loading events'
				: `${records.length}${nextPageToken ? '+' : ''} ${records.length === 1 && !nextPageToken ? 'event' : 'events'}`}</span
		>
		<span aria-hidden="true">·</span>
		<span>{eventFilterSummary(filters)}</span>
	</div>
	{#if recoveryNotice}
		<div class="border-y border-primary/30 bg-primary/10 px-4 py-2 text-xs" role="status">
			{recoveryNotice}
		</div>
	{/if}
	{#if selectionError}
		<div class="border-y border-hairline bg-raised px-4 py-2 text-xs text-text-muted" role="status">
			{selectionError}
		</div>
	{/if}

	{#if error}
		<div
			class="border-y border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
			role="alert"
		>
			{error}
		</div>
	{:else if loading && records.length === 0}
		<div class="grid min-h-64 place-items-center border-y border-hairline text-sm text-text-muted">
			{loadingEarlier ? 'Loading earlier events…' : 'Loading events…'}
		</div>
	{:else if records.length === 0}
		<div class="space-y-3">
			<EventNoResultsState
				clauses={noResultsClauses}
				title="No events found."
				description={`No events found for ${eventFilterSummary(filters)}.`}
				suggestionLabel={noResultsSuggestion?.label}
				onloosen={noResultsSuggestion
					? () => updateFilters(noResultsSuggestion!.update)
					: undefined}
				onclear={clearFilters}
				class="min-h-64 rounded-md border border-dashed border-hairline-strong"
			/>
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
		{#if pageIndex > pageTokenBase || nextPageToken}
			<nav class="flex items-center justify-center gap-3 py-2" aria-label="Event result pages">
				<button
					type="button"
					class="h-9 rounded-sm border border-hairline px-3 text-xs disabled:opacity-40"
					disabled={pageIndex <= pageTokenBase || loading}
					onclick={showNewerEvents}
				>
					Newer events
				</button>
				<span class="font-mono text-2xs text-text-faint">
					{visibleResultStart}-{visibleResultEnd}{nextPageToken ? '+' : ''}
				</span>
				<button
					type="button"
					class="h-9 rounded-sm border border-hairline px-3 text-xs disabled:opacity-40"
					disabled={loadingEarlier || loading || !nextPageToken}
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
