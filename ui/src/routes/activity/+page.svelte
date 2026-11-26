<script lang="ts">
	import { resolve } from '$app/paths';
	import { onDestroy, onMount } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import { useCapabilityState } from '$lib/capability-context';
	import ActivitySummary, { type ActivityUiControls } from '$lib/components/ActivitySummary.svelte';
	import {
		ACTIVITY_ANALYTICS_CAPABILITY,
		isWholeHourUtcOffset,
		type ActivityAnalyticsStatusData,
		type ActivitySummaryData
	} from '$lib/activity-analytics';
	import type { CameraListItem } from '$lib/types';

	const controlClient = useControlClient();
	const capabilities = useCapabilityState();

	let cameras = $state.raw<CameraListItem[]>([]);
	let status = $state.raw<ActivityAnalyticsStatusData | null>(null);
	let summary = $state.raw<ActivitySummaryData | null>(null);
	let loading = $state(true);
	let rebuilding = $state(false);
	let error = $state<string | null>(null);
	let isAdmin = $state(false);
	let supported = $derived(capabilities.supports(ACTIVITY_ANALYTICS_CAPABILITY));
	let timezoneOverridden = false;
	let weekStartOverridden = false;

	let controls = $state<ActivityUiControls>({
		rangePreset: '7d',
		granularity: 'day',
		timezone: '',
		weekStart: 'monday',
		comparison: 'previous_duration',
		cameraId: '',
		eventType: '',
		origin: '',
		zone: '',
		missingZone: false,
		confidenceBand: '',
		image: 'all',
		review: 'all',
		bookmarks: 'all'
	});

	let activeController: AbortController | null = null;
	let requestSeq = 0;

	function rangeWindowMs(preset: ActivityUiControls['rangePreset']): {
		startMs: number;
		endMs: number;
	} {
		const now = Date.now();
		const endMs = Math.ceil(now / 3_600_000) * 3_600_000;
		const spanHours = preset === '24h' ? 24 : preset === '7d' ? 7 * 24 : 30 * 24;
		return { startMs: endMs - spanHours * 3_600_000, endMs };
	}

	async function loadSummary(): Promise<void> {
		if (!supported) {
			loading = false;
			summary = null;
			return;
		}
		activeController?.abort();
		const controller = new AbortController();
		activeController = controller;
		const seq = ++requestSeq;
		loading = true;
		error = null;
		try {
			const [camList, nextStatus] = await Promise.all([
				controlClient.getCameras(),
				controlClient.getActivityAnalyticsStatus(controller.signal)
			]);
			if (controller.signal.aborted || seq !== requestSeq) return;
			cameras = camList;
			status = nextStatus;
			const effectiveTimezone =
				timezoneOverridden && controls.timezone
					? controls.timezone
					: nextStatus.siteTimezone || 'UTC';
			const effectiveWeekStart = weekStartOverridden ? controls.weekStart : nextStatus.weekStart;
			if (controls.timezone !== effectiveTimezone || controls.weekStart !== effectiveWeekStart) {
				controls = {
					...controls,
					timezone: effectiveTimezone,
					weekStart: effectiveWeekStart
				};
			}
			const { startMs, endMs } = rangeWindowMs(controls.rangePreset);
			const includeHeatmap =
				controls.rangePreset !== '30d' || isWholeHourUtcOffset(effectiveTimezone, startMs, endMs);
			const nextSummary = await controlClient.queryActivitySummary({
				startMs,
				endMs,
				timezone: effectiveTimezone,
				granularity: controls.granularity,
				weekStart: effectiveWeekStart,
				comparison: controls.comparison,
				includeHeatmap,
				denseBuckets: true,
				filter: {
					sourceIds: controls.cameraId ? [controls.cameraId] : [],
					eventTypes: controls.eventType ? [controls.eventType] : [],
					origins: controls.origin ? [controls.origin] : [],
					zones: controls.zone && !controls.missingZone ? [controls.zone] : [],
					includeMissingZone: controls.missingZone,
					confidenceBands: controls.confidenceBand ? [controls.confidenceBand] : [],
					image: controls.image,
					review: controls.review,
					bookmarks: controls.bookmarks
				},
				signal: controller.signal
			});
			if (controller.signal.aborted || seq !== requestSeq) return;
			summary = nextSummary;
		} catch (cause) {
			if (controller.signal.aborted || seq !== requestSeq) return;
			error = cause instanceof Error ? cause.message : 'Unable to load activity summary.';
		} finally {
			if (seq === requestSeq) {
				loading = false;
				if (activeController === controller) activeController = null;
			}
		}
	}

	async function triggerRebuild(): Promise<void> {
		rebuilding = true;
		error = null;
		try {
			status = await controlClient.rebuildActivityAnalytics();
			await loadSummary();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Unable to rebuild activity rollups.';
		} finally {
			rebuilding = false;
		}
	}

	onMount(() => {
		const stopAccess = controlClient.onAccessState((access) => {
			isAdmin = access.session?.role === 'administrator';
			if (!access.session) {
				// Clear stale summary data immediately on auth change.
				activeController?.abort();
				summary = null;
				status = null;
				cameras = [];
				timezoneOverridden = false;
				weekStartOverridden = false;
				return;
			}
			void loadSummary();
		});
		return () => {
			stopAccess();
		};
	});

	onDestroy(() => {
		activeController?.abort();
	});
</script>

<svelte:head>
	<title>Activity summary - KeepPeek</title>
</svelte:head>

<div class="h-full overflow-y-auto px-4 py-4 md:p-6">
	{#if !supported}
		<section class="rounded-sm border border-border bg-surface p-6 text-sm">
			<h1 class="text-lg font-semibold">Activity summary</h1>
			<p class="mt-2 text-xs text-text-muted">
				This server does not advertise <code>{ACTIVITY_ANALYTICS_CAPABILITY}</code>. Update the
				recorder to enable bounded event activity summaries.
			</p>
			<a
				href={resolve('/events')}
				class="mt-4 inline-flex min-h-11 items-center rounded-sm border border-border px-3 text-xs hover:bg-raised"
			>
				Return to Events
			</a>
		</section>
	{:else}
		<ActivitySummary
			{summary}
			{status}
			{cameras}
			{controls}
			{loading}
			{error}
			{isAdmin}
			{rebuilding}
			eventsPath={resolve('/events')}
			onControlsChange={(next) => {
				if (next.timezone !== controls.timezone) timezoneOverridden = true;
				if (next.weekStart !== controls.weekStart) weekStartOverridden = true;
				controls = next;
				void loadSummary();
			}}
			onRefresh={() => void loadSummary()}
			onRebuild={() => void triggerRebuild()}
		/>
	{/if}
</div>
