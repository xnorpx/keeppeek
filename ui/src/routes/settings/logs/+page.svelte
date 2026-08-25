<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { browserLogStore } from '$lib/browser-logs';
	import LogViewer from '$lib/components/logging/LogViewer.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import { downloadDiagnosticsBundle } from '$lib/diagnostics-bundle';
	import { downloadBugReport } from '$lib/log-export';
	import { useControlClient } from '$lib/control-context';
	import { ServerLogStream, type LogStreamState } from '$lib/server-log-stream';
	import type { BrowserLogEntry, LoggingSettings, LogSnapshot, ServerLogEntry } from '$lib/types';
	import ArrowLeftIcon from '@lucide/svelte/icons/arrow-left';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import SaveIcon from '@lucide/svelte/icons/save';

	type LogTab = 'server' | 'browser';

	let activeTab = $state<LogTab>('server');
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let settings = $state.raw<LoggingSettings | null>(null);
	let filterDraft = $state('');
	let filterError = $state<string | null>(null);
	let savingFilter = $state(false);
	let downloading = $state(false);
	let downloadingDiagnostics = $state(false);
	let exportError = $state<string | null>(null);
	let serverEntries = $state.raw<ServerLogEntry[]>([]);
	let browserEntries = $state.raw<BrowserLogEntry[]>([]);
	let streamState = $state<LogStreamState>('closed');
	let skippedEntries = $state(0);
	let stream: ServerLogStream | null = null;
	const controlClient = useControlClient();

	onMount(() => {
		let active = true;
		browserEntries = browserLogStore.snapshot();
		const unsubscribe = browserLogStore.subscribe(() => {
			if (active) browserEntries = browserLogStore.snapshot();
		});

		void loadLogging().catch(() => {});
		const handleVisibility = () => {
			if (document.visibilityState === 'hidden') {
				stream?.close();
				return;
			}
			startStream(serverEntries.at(-1)?.sequence);
		};
		document.addEventListener('visibilitychange', handleVisibility);
		return () => {
			active = false;
			unsubscribe();
			stream?.close(false);
			document.removeEventListener('visibilitychange', handleVisibility);
		};
	});

	async function loadLogging(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			const nextSettings = await controlClient.getLoggingSettings();
			settings = nextSettings;
			filterDraft = nextSettings.active_filter;
			startStream(undefined, 1_000);
		} catch (cause) {
			loadError = cause instanceof Error ? cause.message : 'Unable to load logs.';
			throw cause;
		} finally {
			loading = false;
		}
	}

	function startStream(after?: number, tail = 200): void {
		stream?.close(false);
		stream = new ServerLogStream({
			onentry: appendServerEntry,
			onstate: (state) => (streamState = state),
			ongap: (dropped) => (skippedEntries += dropped),
			onreplaytruncated: () => (skippedEntries += 1)
		});
		stream.start(after, tail);
	}

	function appendServerEntry(entry: ServerLogEntry): void {
		if (serverEntries.some((existing) => existing.sequence === entry.sequence)) return;
		serverEntries = [...serverEntries, entry].slice(-10_000);
	}

	async function saveFilter(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (savingFilter) return;
		savingFilter = true;
		filterError = null;
		try {
			settings = await controlClient.setLoggingFilter(filterDraft);
			filterDraft = settings.active_filter;
		} catch (cause) {
			filterError = cause instanceof Error ? cause.message : 'Unable to update the log filter.';
		} finally {
			savingFilter = false;
		}
	}

	async function exportLogs(): Promise<void> {
		if (!settings || downloading) return;
		downloading = true;
		exportError = null;
		try {
			downloadBugReport({
				settings,
				server: visibleServerSnapshot(settings),
				browser: browserLogStore.snapshot(),
				viewerFilters: { active_tab: activeTab }
			});
		} catch (cause) {
			exportError = cause instanceof Error ? cause.message : 'Unable to export logs.';
		} finally {
			downloading = false;
		}
	}

	async function exportDiagnostics(): Promise<void> {
		if (downloadingDiagnostics) return;
		downloadingDiagnostics = true;
		exportError = null;
		try {
			await downloadDiagnosticsBundle(controlClient, browserLogStore.snapshot());
		} catch (cause) {
			exportError =
				cause instanceof Error ? cause.message : 'Unable to create the diagnostics package.';
		} finally {
			downloadingDiagnostics = false;
		}
	}

	function visibleServerSnapshot(currentSettings: LoggingSettings): LogSnapshot {
		return {
			entries: serverEntries,
			oldest_sequence: serverEntries[0]?.sequence ?? null,
			newest_sequence: serverEntries.at(-1)?.sequence ?? null,
			truncated: skippedEntries > 0,
			stats: currentSettings.buffer
		};
	}

	function clearServerView(): void {
		serverEntries = [];
		skippedEntries = 0;
	}

	function clearBrowserLogs(): void {
		browserLogStore.clear();
	}
</script>

<svelte:head>
	<title>Logs - KeepPeek</title>
</svelte:head>

<div class="mx-auto max-w-[110rem] space-y-5">
	<header class="flex flex-wrap items-start justify-between gap-3">
		<div class="flex items-start gap-3">
			<Button
				href={resolve('/settings')}
				variant="outline"
				size="icon"
				aria-label="Back to settings"
			>
				<ArrowLeftIcon />
			</Button>
			<div>
				<h1 class="text-2xl font-bold tracking-tight">Logs</h1>
				<p class="mt-1 text-sm text-muted-foreground">Live server and browser diagnostics</p>
			</div>
		</div>
		<div class="flex items-center gap-3">
			{#if settings}
				<div class="text-right text-xs text-muted-foreground">
					<p>KeepPeek {settings.version}</p>
					<p>{settings.buffer.entry_count.toLocaleString()} retained server entries</p>
				</div>
			{/if}
			<Button
				variant="outline"
				disabled={!settings || downloadingDiagnostics}
				onclick={() => void exportDiagnostics()}
			>
				<DownloadIcon class={downloadingDiagnostics ? 'animate-pulse' : undefined} />
				{downloadingDiagnostics ? 'Building package' : 'Download diagnostics'}
			</Button>
		</div>
	</header>

	{#if loading}
		<div class="grid h-64 place-items-center border-y text-sm text-muted-foreground" role="status">
			Loading logs…
		</div>
	{:else if loadError}
		<div
			class="flex flex-wrap items-center justify-between gap-3 border-y border-destructive/35 bg-destructive/5 px-4 py-3"
			role="alert"
		>
			<p class="text-sm text-destructive">{loadError}</p>
			<Button variant="outline" onclick={() => void loadLogging()}>Retry</Button>
		</div>
	{:else}
		<div class="flex border-b" role="tablist" aria-label="Log source">
			<button
				id="server-log-tab"
				type="button"
				role="tab"
				aria-selected={activeTab === 'server'}
				aria-controls="server-log-panel"
				class="border-b-2 px-4 py-2 text-sm font-medium {activeTab === 'server'
					? 'border-primary text-foreground'
					: 'border-transparent text-muted-foreground hover:text-foreground'}"
				onclick={() => (activeTab = 'server')}
			>
				Server
			</button>
			<button
				id="browser-log-tab"
				type="button"
				role="tab"
				aria-selected={activeTab === 'browser'}
				aria-controls="browser-log-panel"
				class="border-b-2 px-4 py-2 text-sm font-medium {activeTab === 'browser'
					? 'border-primary text-foreground'
					: 'border-transparent text-muted-foreground hover:text-foreground'}"
				onclick={() => (activeTab = 'browser')}
			>
				Browser / Svelte
			</button>
		</div>

		{#if activeTab === 'server'}
			<div id="server-log-panel" role="tabpanel" class="space-y-4" aria-labelledby="server-log-tab">
				<form
					class="flex flex-col gap-2 border-y bg-muted/25 px-3 py-3 lg:flex-row lg:items-end"
					onsubmit={saveFilter}
				>
					<label class="grid min-w-0 flex-1 gap-1.5 text-sm font-medium" for="server-log-filter">
						Server capture filter
						<Input
							id="server-log-filter"
							bind:value={filterDraft}
							spellcheck="false"
							autocomplete="off"
							placeholder="info,str0m=warn"
						/>
					</label>
					<Button type="submit" disabled={savingFilter}>
						<SaveIcon />
						{savingFilter ? 'Saving' : 'Save filter'}
					</Button>
					{#if settings}
						<p class="pb-2 text-xs text-muted-foreground lg:max-w-xs">
							Active: <span class="font-mono text-foreground">{settings.active_filter}</span>
						</p>
					{/if}
				</form>
				{#if filterError}
					<p class="text-sm text-destructive" role="alert">{filterError}</p>
				{:else if settings?.filter_error}
					<p class="text-sm text-amber-700" role="status">{settings.filter_error}</p>
				{/if}
				<LogViewer
					entries={serverEntries}
					connection={streamState}
					skipped={skippedEntries}
					onclear={clearServerView}
					ondownload={() => void exportLogs()}
					{downloading}
				/>
			</div>
		{:else}
			<div
				id="browser-log-panel"
				role="tabpanel"
				class="space-y-4"
				aria-labelledby="browser-log-tab"
			>
				<LogViewer
					entries={browserEntries}
					connection="current tab session"
					onclear={clearBrowserLogs}
					ondownload={() => void exportLogs()}
					{downloading}
				/>
			</div>
		{/if}

		{#if exportError}
			<p class="text-sm text-destructive" role="alert">{exportError}</p>
		{/if}
	{/if}
</div>
