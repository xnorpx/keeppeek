<script lang="ts">
	import { onDestroy } from 'svelte';
	import { setAppearanceState } from '$lib/appearance-context';
	import AppearanceSystemSection from '$lib/components/AppearanceSystemSection.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import LogViewer from '$lib/components/logging/LogViewer.svelte';
	import ReportBugButton from '$lib/components/ReportBugButton.svelte';
	import ReportBugWorkflow from '$lib/components/ReportBugWorkflow.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { createDiagnosticRedactor } from '$lib/diagnostic-redaction';
	import { provideReportContext } from '$lib/report-context';
	import { ReportSession } from '$lib/report-session.svelte';
	import type { ReportTransport } from '$lib/report-submission';
	import type { BrowserLogEntry, ServerHealthResponse, ServerLogEntry } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import SunMoonIcon from '@lucide/svelte/icons/sun-moon';

	type View = 'appearance' | 'logs';

	const baseTimeMs = Date.UTC(2026, 7, 18, 6, 37, 0);

	const health = {
		version: '0.4.1-pre · 9f2ac1d',
		uptime_seconds: 1_836_840,
		system: {
			host_name: 'keeppeek-nvr-01',
			os_name: 'macOS',
			os_version: 'macOS 15.4',
			process: {
				name: 'Native service',
				executable: '/opt/keeppeek/bin/keeppeek',
				working_directory: '/opt/keeppeek'
			}
		}
	} as ServerHealthResponse;

	const initialServerEntries: ServerLogEntry[] = [
		{
			sequence: 1,
			timestamp_ms: baseTimeMs + 1_100,
			level: 'info',
			target: 'keeppeek::events',
			message: 'person 0.91 front-door persisted revision 2',
			fields: { source_id: 'front-door', confidence: 0.91 }
		},
		{
			sequence: 2,
			timestamp_ms: baseTimeMs + 4_800,
			level: 'info',
			target: 'keeppeek::webrtc',
			message: 'session 7c1a negotiating 4 live video tracks',
			fields: { session: '7c1a', tracks: 4 }
		},
		{
			sequence: 3,
			timestamp_ms: baseTimeMs + 9_200,
			level: 'debug',
			target: 'keeppeek::rtsp',
			message: 'porch keepalive OPTIONS 200 OK (11 ms)',
			fields: { camera: 'porch', rtt_ms: 11 }
		},
		{
			sequence: 4,
			timestamp_ms: baseTimeMs + 14_118,
			level: 'error',
			target: 'keeppeek::rtsp',
			message: 'back-yard connect refused; scheduling 30s backoff (try 4)',
			fields: { camera: 'back-yard', backoff_s: 30 }
		},
		{
			sequence: 5,
			timestamp_ms: baseTimeMs + 18_441,
			level: 'warn',
			target: 'keeppeek::rtsp',
			message: 'porch rtp gap 41022..41029 detected; awaiting next keyframe',
			fields: { camera: 'porch', gap_packets: 8 }
		},
		{
			sequence: 6,
			timestamp_ms: baseTimeMs + 20_902,
			level: 'info',
			target: 'keeppeek::storage',
			message: 'fragment flushed 4.2 MiB to recordings store',
			fields: { bytes: 4_404_019 }
		},
		{
			sequence: 7,
			timestamp_ms: baseTimeMs + 22_310,
			level: 'warn',
			target: 'keeppeek::rtsp',
			message: 'porch keyframe recovered after 1.4s; recording continuous',
			fields: { camera: 'porch', recovery_ms: 1400 }
		},
		{
			sequence: 8,
			timestamp_ms: baseTimeMs + 23_114,
			level: 'info',
			target: 'keeppeek::rtsp',
			message: 'front-door keyframe interval nominal 1.1s',
			fields: { camera: 'front-door' }
		}
	];

	const browserEntries: BrowserLogEntry[] = [
		{
			sequence: 1,
			timestamp_ms: baseTimeMs + 12_000,
			level: 'info',
			target: 'browser.ui',
			message: 'opened settings logs viewer',
			fields: {},
			source: 'console'
		},
		{
			sequence: 2,
			timestamp_ms: baseTimeMs + 19_000,
			level: 'warn',
			target: 'browser.webrtc',
			message: 'porch video decoder waited 1.4s for keyframe',
			fields: {},
			source: 'console'
		}
	];

	function createDemoReportSession(): ReportSession {
		let checkpointCall = 0;
		const redactor = createDiagnosticRedactor([{ value: 'porch-cam', replacement: 'camera-001' }]);
		const diagnosticsBlob = new Blob([new Uint8Array(24_576)], { type: 'application/gzip' });

		return new ReportSession({
			now: () => Date.now(),
			latestBrowserSequence: () => (checkpointCall === 0 ? 0 : browserEntries.length),
			browserEntries: () => browserEntries,
			async snapshot(position) {
				const isStart = checkpointCall === 0;
				checkpointCall += 1;
				return {
					cursor: {
						utcMs: position.utcMs,
						browserSequence: isStart ? 0 : browserEntries.length,
						server: {
							sequence: isStart ? 0 : initialServerEntries.length,
							generation: 'gen-2026-08-18',
							filter: 'info,keeppeek::rtsp=debug'
						}
					},
					server: isStart
						? null
						: {
								entries: initialServerEntries,
								oldest_sequence: 1,
								newest_sequence: initialServerEntries.length,
								truncated: false,
								generation: 'gen-2026-08-18',
								stats: {
									entry_count: initialServerEntries.length,
									byte_count: 2_048,
									evicted_entries: 0,
									max_entries: 10_000,
									max_bytes: 4_194_304,
									active_streams: 1,
									max_streams: 4
								}
							}
				};
			},
			async package() {
				return {
					diagnostics: diagnosticsBlob,
					redactText: redactor.text,
					warnings: []
				};
			},
			async capture() {
				throw new Error('Screen capture is skipped in logs-only demo');
			}
		});
	}

	// ponytail: non-network ReportTransport stub unlocks the real redaction review step without HTTP I/O.
	const mockTransport: ReportTransport = {
		async create() {
			throw new Error('Remote submission is not invoked in offline demo');
		},
		async status() {
			return { state: 'pending' };
		},
		async upload() {},
		async finalize() {
			return { state: 'pending' };
		},
		async cancel() {}
	};

	const appearance = setAppearanceState();
	const reportSession = createDemoReportSession();

	let view = $state<View>('appearance');
	let serverEntries = $state.raw<ServerLogEntry[]>(initialServerEntries);
	let diagnosticsNotice = $state<string | null>(null);
	let reportOpen = $state(false);

	provideReportContext({
		session: reportSession,
		show: () => {
			reportOpen = true;
		},
		authorized: () => true
	});

	function prepareDiagnostics(): void {
		diagnosticsNotice =
			'Prepared keeppeek-diagnostics-2026-08-18T06-37-23Z.json.gz (142.4 KiB · privacy: scrubbed)';
	}

	function clearLogs(): void {
		serverEntries = [];
	}

	onDestroy(() => {
		reportSession.cancel();
		appearance.setPreference('dark');
	});
</script>

<main
	data-paper-scenario="settings.desktop.appearance-system-logs"
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="settings" />

	<section class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header
			class="flex h-14 shrink-0 items-center justify-between border-b border-hairline bg-surface px-6"
		>
			<div class="flex items-baseline gap-3">
				<h1 class="text-base font-semibold">Settings</h1>
				<span class="font-mono text-2xs tracking-caps text-text-faint">
					APPEARANCE · SYSTEM · LOGS &amp; DIAGNOSTICS
				</span>
			</div>
			<nav class="flex items-center gap-1.5" aria-label="Settings diagnostics views">
				<button
					type="button"
					data-demo-action="view-appearance"
					aria-pressed={view === 'appearance'}
					class="inline-flex h-8 items-center gap-1.5 rounded-sm border px-3 text-xs font-medium {view ===
					'appearance'
						? 'border-primary bg-primary text-on-primary'
						: 'border-hairline bg-raised text-text-muted'}"
					onclick={() => (view = 'appearance')}
				>
					<SunMoonIcon class="size-3.5" /> Appearance &amp; system
				</button>
				<button
					type="button"
					data-demo-action="view-logs"
					aria-pressed={view === 'logs'}
					class="inline-flex h-8 items-center gap-1.5 rounded-sm border px-3 text-xs font-medium {view ===
					'logs'
						? 'border-primary bg-primary text-on-primary'
						: 'border-hairline bg-raised text-text-muted'}"
					onclick={() => (view = 'logs')}
				>
					<FileTextIcon class="size-3.5" /> Logs
				</button>
			</nav>
		</header>

		<div class="min-h-0 flex-1 overflow-y-auto p-5">
			{#if view === 'appearance'}
				<AppearanceSystemSection
					{health}
					timeZoneOverride="Europe/Stockholm"
					reducedMotionOverride={false}
					onrestart={() => {}}
					ondownloaddiagnostics={prepareDiagnostics}
				/>
			{:else}
				<div class="space-y-4">
					<header class="flex flex-wrap items-center justify-between gap-3">
						<div>
							<h2 class="text-xl font-semibold tracking-tight">Logs</h2>
							<p class="mt-0.5 text-xs text-text-muted">
								Live server stream · Active filter
								<code class="font-mono text-foreground">info,keeppeek::rtsp=debug</code>
							</p>
						</div>
						<div class="flex items-center gap-2">
							<Button
								variant="outline"
								size="sm"
								data-demo-action="download-diagnostics"
								onclick={prepareDiagnostics}
							>
								<DownloadIcon /> Download diagnostics
							</Button>
							<ReportBugButton />
						</div>
					</header>

					{#if diagnosticsNotice}
						<p
							role="status"
							data-demo-diagnostics-ready="true"
							class="flex items-center gap-2 rounded-sm border border-healthy/40 bg-healthy/10 px-3 py-2 text-xs text-healthy"
						>
							<CheckIcon class="size-4 shrink-0" />
							{diagnosticsNotice}
						</p>
					{/if}

					<LogViewer
						entries={serverEntries}
						connection="live"
						onclear={clearLogs}
						ondownload={prepareDiagnostics}
					/>
				</div>
			{/if}
		</div>
	</section>

	<ReportBugWorkflow
		session={reportSession}
		bind:open={reportOpen}
		reporting={{ transport: mockTransport, retentionDays: 7 }}
		onaudit={() => {}}
	/>
</main>
