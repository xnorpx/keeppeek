<script lang="ts">
	import { resolve } from '$app/paths';
	import RecordingCoverageDetail from '$lib/components/RecordingCoverageDetail.svelte';
	import { ApiRequestError } from '$lib/api';
	import { useControlClient } from '$lib/control-context';
	import {
		coverageStateLabel,
		formatBytes,
		formatDuration,
		formatPercent,
		summarizeCamera
	} from '$lib/recording-coverage';
	import type {
		CameraRecordingCoverage,
		RecordingCoverageResponse,
		RecordingCoverageState
	} from '$lib/types';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import ArchiveIcon from '@lucide/svelte/icons/archive';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SearchIcon from '@lucide/svelte/icons/search';
	import { onMount } from 'svelte';

	const controlClient = useControlClient();
	const rangeOptions = [
		{ value: 1, label: '24h' },
		{ value: 7, label: '7d' },
		{ value: 30, label: '30d' }
	] as const;

	let snapshot = $state.raw<RecordingCoverageResponse | null>(null);
	let loading = $state(true);
	let refreshing = $state(false);
	let error: string | null = $state(null);
	let search = $state('');
	let stateFilter = $state<RecordingCoverageState | ''>('');
	let streamFilter = $state<'main' | 'sub' | ''>('');
	let groupFilter = $state('');
	let rangeDays = $state(1);
	let minimumCameraGapMs = $state(0);
	let selectedCameraId = $state('');
	let pageIndex = $state(0);
	let pageTokens = $state.raw<(string | null)[]>([null]);
	let refreshRequest = $state(0);
	let requestController: AbortController | null = null;
	let selectedCamera = $derived(
		snapshot?.cameras.find((camera) => camera.camera_id === selectedCameraId) ??
			snapshot?.cameras[0] ??
			null
	);
	let filterKey = $derived(
		`${search}\u0000${stateFilter}\u0000${streamFilter}\u0000${groupFilter}\u0000${rangeDays}\u0000${minimumCameraGapMs}\u0000${refreshRequest}`
	);

	$effect(() => {
		void filterKey;
		const timer = window.setTimeout(() => void loadCoverage(true), search.trim() ? 250 : 0);
		return () => window.clearTimeout(timer);
	});

	onMount(() => {
		const timer = window.setInterval(() => (refreshRequest += 1), 30_000);
		return () => {
			window.clearInterval(timer);
			requestController?.abort();
		};
	});

	async function loadCoverage(resetPage: boolean): Promise<void> {
		if (resetPage) {
			pageIndex = 0;
			pageTokens = [null];
		}
		requestController?.abort();
		const controller = new AbortController();
		requestController = controller;
		if (snapshot) refreshing = true;
		else loading = true;
		try {
			const now = Date.now();
			const pageToken = pageTokens[pageIndex];
			const next = await controlClient.getRecordingCoverage(
				pageToken
					? { pageToken }
					: {
							startMs: now - rangeDays * 86_400_000,
							endMs: now,
							minimumGapMs: 5_000,
							minimumCameraGapMs: minimumCameraGapMs || undefined,
							pageSize: 25,
							search: search.trim() || undefined,
							state: stateFilter || undefined,
							stream: streamFilter || undefined,
							group: groupFilter || undefined
						},
				controller.signal
			);
			if (controller.signal.aborted) return;
			snapshot = next;
			pageTokens = [
				...pageTokens.slice(0, pageIndex + 1),
				...(next.next_page_token ? [next.next_page_token] : [])
			];
			error = null;
		} catch (cause) {
			if (controller.signal.aborted) return;
			if (cause instanceof ApiRequestError && cause.status === 409 && !resetPage) {
				pageIndex = 0;
				pageTokens = [null];
				queueMicrotask(() => void loadCoverage(true));
				return;
			}
			error = cause instanceof Error ? cause.message : 'Recording coverage is unavailable.';
		} finally {
			if (requestController === controller) {
				loading = false;
				refreshing = false;
			}
		}
	}

	function selectCamera(camera: CameraRecordingCoverage): void {
		selectedCameraId = camera.camera_id;
	}

	function stateTone(state: RecordingCoverageState): string {
		if (state === 'healthy') return 'bg-healthy';
		if (state === 'degraded') return 'bg-live';
		if (state === 'paused_by_policy') return 'bg-activity';
		return 'bg-text-faint';
	}

	function previousPage(): void {
		if (pageIndex === 0) return;
		pageIndex -= 1;
		void loadCoverage(false);
	}

	function nextPage(): void {
		if (!snapshot?.next_page_token) return;
		pageIndex += 1;
		void loadCoverage(false);
	}
</script>

<svelte:head>
	<title>Recording integrity - KeepPeek</title>
</svelte:head>

<div data-recording-dashboard class="flex min-h-0 flex-col bg-ground">
	<header class="flex min-h-14 flex-wrap items-center gap-3 border-b border-hairline px-4 py-2.5">
		<div class="min-w-0">
			<h1 class="text-lg font-semibold">Recording integrity</h1>
			<p class="font-mono text-2xs text-text-faint">
				{snapshot
					? `CATALOG REV ${snapshot.catalog_revision} · ${rangeDays === 1 ? '24 HOURS' : `${rangeDays} DAYS`}`
					: 'CATALOG EVIDENCE'}
			</p>
		</div>
		<div class="min-w-2 flex-1"></div>
		<a
			href={resolve('/keep')}
			class="inline-flex h-9 items-center gap-2 rounded-sm border border-hairline-strong px-3 text-xs text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
		>
			<ArchiveIcon class="size-3.5" /> Open Keep
		</a>
		<button
			type="button"
			class="grid size-11 place-items-center rounded-sm border border-hairline-strong text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40 md:size-9"
			title="Refresh recording evidence"
			aria-label="Refresh recording evidence"
			disabled={refreshing}
			onclick={() => (refreshRequest += 1)}
		>
			<RefreshCwIcon class="size-3.5 {refreshing ? 'animate-spin' : ''}" />
		</button>
	</header>

	{#if snapshot}
		<section
			class="grid grid-cols-2 border-b border-hairline md:grid-cols-4 {snapshot.totals
				.not_configured > 0
				? 'xl:grid-cols-8'
				: 'xl:grid-cols-7'}"
			aria-label="Fleet recording summary"
		>
			{#each [['HEALTHY', snapshot.totals.healthy, 'text-healthy', ''], ['DEGRADED', snapshot.totals.degraded, 'text-live-text', ''], ['POLICY PAUSED', snapshot.totals.paused_by_policy, 'text-activity', ''], ...(snapshot.totals.not_configured > 0 ? [['NOT CONFIGURED', snapshot.totals.not_configured, 'text-text-muted', '']] : []), ['UNKNOWN', snapshot.totals.unknown, 'text-text-muted', ''], ['STORED', formatBytes(snapshot.storage.recording_bytes), 'text-foreground', 'hidden md:block'], ['GROWTH / DAY', formatBytes(snapshot.storage.estimated_bytes_per_day), 'text-foreground', 'hidden md:block'], ['PROJECTED', snapshot.storage.projected_retention_days === null ? 'No estimate' : `${snapshot.storage.projected_retention_days.toFixed(1)}d`, 'text-foreground', 'hidden md:block']] as item (item[0])}
				<div
					data-recording-summary-metric
					class="min-w-0 border-r border-b border-hairline px-3 py-3 last:border-r-0 md:px-4 {item[3]}"
				>
					<p class="font-mono text-[10px] tracking-caps text-text-faint">{item[0]}</p>
					<p class="mt-1 truncate text-xl font-semibold tabular-nums {item[2]}">{item[1]}</p>
				</div>
			{/each}
		</section>
	{/if}

	<section
		class="flex flex-wrap items-center gap-2 border-b border-hairline bg-surface px-4 py-2.5"
		aria-label="Recording filters"
	>
		<label class="relative min-w-48 flex-1 md:max-w-72">
			<SearchIcon
				class="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-text-faint"
			/>
			<span class="sr-only">Search cameras</span>
			<input
				type="search"
				class="h-9 w-full rounded-sm border border-hairline-strong bg-raised pr-3 pl-8 text-xs outline-none placeholder:text-text-faint focus:border-ring focus:ring-1 focus:ring-ring"
				placeholder="Search cameras"
				bind:value={search}
			/>
		</label>
		<div
			class="flex h-9 rounded-sm border border-hairline-strong bg-raised p-0.5"
			aria-label="Coverage interval"
		>
			{#each rangeOptions as option (option.value)}
				<button
					type="button"
					class="min-w-11 rounded-xs px-2 text-xs focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {rangeDays ===
					option.value
						? 'bg-primary text-primary-foreground'
						: 'text-text-muted'}"
					aria-pressed={rangeDays === option.value}
					onclick={() => (rangeDays = option.value)}>{option.label}</button
				>
			{/each}
		</div>
		<label>
			<span class="sr-only">Recording state</span>
			<select
				class="h-9 rounded-sm border border-hairline-strong bg-raised px-2 text-xs outline-none focus:border-ring"
				bind:value={stateFilter}
			>
				<option value="">All states</option>
				<option value="healthy">Healthy</option>
				<option value="degraded">Degraded</option>
				<option value="paused_by_policy">Policy paused</option>
				<option value="not_configured">Not configured</option>
				<option value="unknown">Unknown</option>
			</select>
		</label>
		<label>
			<span class="sr-only">Recording stream</span>
			<select
				class="h-9 rounded-sm border border-hairline-strong bg-raised px-2 text-xs outline-none focus:border-ring"
				bind:value={streamFilter}
			>
				<option value="">All streams</option>
				<option value="main">Main</option>
				<option value="sub">Sub</option>
			</select>
		</label>
		{#if snapshot?.groups.length}
			<label>
				<span class="sr-only">Camera group</span>
				<select
					class="h-9 rounded-sm border border-hairline-strong bg-raised px-2 text-xs outline-none focus:border-ring"
					bind:value={groupFilter}
				>
					<option value="">All groups</option>
					{#each snapshot.groups as group (group)}
						<option value={group}>{group}</option>
					{/each}
				</select>
			</label>
		{/if}
		<label>
			<span class="sr-only">Minimum camera gap</span>
			<select
				class="h-9 rounded-sm border border-hairline-strong bg-raised px-2 text-xs outline-none focus:border-ring"
				value={minimumCameraGapMs}
				onchange={(event) => (minimumCameraGapMs = Number(event.currentTarget.value))}
			>
				<option value={0}>All gap sizes</option>
				<option value={60_000}>Gap ≥ 1m</option>
				<option value={300_000}>Gaps ≥ 5m</option>
				<option value={900_000}>Gaps ≥ 15m</option>
				<option value={3_600_000}>Gaps ≥ 1h</option>
			</select>
		</label>
	</section>

	{#if error}
		<div
			class="flex items-center gap-3 border-b border-live/40 bg-live/10 px-4 py-3 text-sm text-live-text"
			role="alert"
		>
			<ActivityIcon class="size-4 shrink-0" />
			<span class="min-w-0 flex-1">{error}</span>
			<button
				type="button"
				class="h-8 rounded-sm border border-live/40 px-3 text-xs"
				onclick={() => (refreshRequest += 1)}>Retry</button
			>
		</div>
	{/if}

	{#if snapshot?.findings.length}
		<section class="border-b border-hairline bg-raised" aria-label="Priority recording findings">
			{#each snapshot.findings.slice(0, 3) as finding (`${finding.camera_id}-${finding.stream_id}-${finding.kind}`)}
				<a
					href={finding.playback_href ?? finding.health_href}
					class="grid min-h-10 grid-cols-[8px_minmax(0,1fr)_auto] items-center gap-3 border-b border-hairline px-4 text-xs last:border-b-0 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
				>
					<span
						class="size-2 rounded-full {finding.severity === 'critical'
							? 'bg-live'
							: 'bg-activity'}"
					></span>
					<span class="truncate"><strong>{finding.camera_name}</strong> · {finding.message}</span>
					<span class="font-mono text-[10px] text-text-faint uppercase"
						>{finding.stream_id ?? 'camera'}</span
					>
				</a>
			{/each}
		</section>
	{/if}

	<div class="grid min-h-0 flex-1 xl:grid-cols-[minmax(620px,1.35fr)_minmax(420px,1fr)]">
		<section class="min-w-0" aria-labelledby="recording-fleet-heading">
			<header
				class="grid h-[34px] grid-cols-[minmax(150px,1.3fr)_110px_90px_100px_90px] items-center gap-3 border-b border-hairline bg-surface px-4 font-mono text-[10px] tracking-caps text-text-faint max-md:hidden"
			>
				<h2 id="recording-fleet-heading" class="font-mono font-normal">CAMERA</h2>
				<span>WRITER</span><span>COVERAGE</span><span>RETENTION</span><span class="text-right"
					>GAPS</span
				>
			</header>
			{#if loading && !snapshot}
				<div class="space-y-px bg-hairline" aria-label="Loading recording coverage">
					{#each Array.from({ length: 8 }) as _, index (index)}
						<div class="h-[72px] animate-pulse bg-surface px-4 py-3">
							<span class="block h-3 w-1/3 rounded-sm bg-raised"></span><span
								class="mt-3 block h-2 w-2/3 rounded-sm bg-raised"
							></span>
						</div>
					{/each}
				</div>
			{:else if snapshot && snapshot.cameras.length === 0}
				<div
					class="grid min-h-52 place-items-center border-b border-hairline px-6 text-center text-sm text-text-muted"
				>
					No cameras match the selected recording filters.
				</div>
			{:else if snapshot}
				<div data-recording-fleet-list>
					{#each snapshot.cameras as camera (camera.camera_id)}
						{@const summary = summarizeCamera(camera)}
						<button
							type="button"
							data-recording-camera={camera.camera_id}
							class="grid min-h-[72px] w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b border-hairline px-4 py-3 text-left focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset md:min-h-14 md:grid-cols-[minmax(150px,1.3fr)_110px_90px_100px_90px] md:py-2 {selectedCamera?.camera_id ===
							camera.camera_id
								? 'bg-raised'
								: 'bg-surface hover:bg-raised/60'}"
							aria-pressed={selectedCamera?.camera_id === camera.camera_id}
							onclick={() => selectCamera(camera)}
						>
							<div class="min-w-0">
								<div class="flex items-center gap-2">
									<span class="size-2 shrink-0 rounded-full {stateTone(camera.state)}"></span><span
										class="truncate text-sm font-semibold">{camera.camera_name}</span
									>
								</div>
								<p class="mt-1 truncate font-mono text-[10px] text-text-faint uppercase">
									{camera.camera_id} · {camera.policy}
								</p>
							</div>
							<span class="text-right text-xs text-text-muted md:text-left"
								>{coverageStateLabel(camera.state)}</span
							>
							<span class="hidden font-mono text-xs tabular-nums md:block"
								>{formatPercent(summary.coveragePercent)}</span
							>
							<span class="hidden font-mono text-xs tabular-nums md:block"
								>{formatDuration(summary.effectiveRetentionMs)}</span
							>
							<span class="hidden text-right font-mono text-xs tabular-nums md:block"
								>{summary.gapCount}</span
							>
							<div
								class="col-span-2 flex flex-wrap gap-x-4 font-mono text-[10px] text-text-faint md:hidden"
							>
								<span>{formatPercent(summary.coveragePercent)} coverage</span><span
									>{formatDuration(summary.effectiveRetentionMs)} retained</span
								><span>{summary.gapCount} gaps</span><span
									>{formatBytes(summary.recordingBytes)}</span
								>
							</div>
						</button>
					{/each}
				</div>
				<footer
					class="flex h-12 items-center justify-between border-b border-hairline bg-surface px-4"
				>
					<span class="font-mono text-2xs text-text-faint"
						>PAGE {pageIndex + 1} · {snapshot.totals.cameras} CAMERAS</span
					>
					<div class="flex items-center gap-1">
						<button
							type="button"
							class="grid size-11 place-items-center rounded-sm border border-hairline-strong focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-30 md:size-9"
							aria-label="Previous camera page"
							title="Previous camera page"
							disabled={pageIndex === 0 || refreshing}
							onclick={previousPage}><ChevronLeftIcon class="size-4" /></button
						>
						<button
							type="button"
							class="grid size-11 place-items-center rounded-sm border border-hairline-strong focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-30 md:size-9"
							aria-label="Next camera page"
							title="Next camera page"
							disabled={!snapshot.next_page_token || refreshing}
							onclick={nextPage}><ChevronRightIcon class="size-4" /></button
						>
					</div>
				</footer>
			{/if}
		</section>

		{#if selectedCamera && snapshot}
			<RecordingCoverageDetail
				camera={selectedCamera}
				windowStartMs={snapshot.window.start_ms}
				windowEndMs={snapshot.window.end_ms}
				nowMs={snapshot.generated_at_ms}
			/>
		{:else}
			<section
				class="hidden border-l border-hairline bg-surface xl:block"
				aria-label="Camera recording detail"
			></section>
		{/if}
	</div>
</div>
