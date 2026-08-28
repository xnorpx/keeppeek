<script module lang="ts">
	function compact(value: number): string {
		return new Intl.NumberFormat(undefined, {
			notation: 'compact',
			maximumFractionDigits: 1
		}).format(value);
	}
</script>

<script lang="ts">
	import { resolve } from '$app/paths';
	import { rankHealthFindings } from '$lib/health-presentation';
	import type { CameraHealth, ServerHealthResponse, StreamHealth } from '$lib/types';
	import DesktopPaperRail from './DesktopPaperRail.svelte';
	import HealthPriorityCard from './HealthPriorityCard.svelte';

	export type BrowserHealthEvidence = {
		roundTripMs: number | null;
		jitterMs: number | null;
		packetLossPercent: number | null;
		framesDropped: number | null;
		connection: string;
		decoder: string;
		presented: string;
		quality: string;
	};

	type Props = {
		health: ServerHealthResponse;
		browser: BrowserHealthEvidence;
		paperFrame?: boolean;
	};
	type HealthStreamRow = {
		camera: CameraHealth;
		stream: StreamHealth | null;
	};

	let { health, browser, paperFrame = false }: Props = $props();
	let findings = $derived(rankHealthFindings(health));
	let primaryFinding = $derived(findings[0] ?? null);
	let recordingDisk = $derived(health.system.disks.find((disk) => disk.stores_recordings) ?? null);
	let fleetCounts = $derived({
		configured: health.totals.configured_cameras,
		connected: health.totals.connected_cameras ?? null,
		fresh: health.totals.fresh_cameras ?? null,
		decodable: health.totals.decodable_cameras ?? null,
		recording: health.totals.recording_cameras ?? null
	});
	let streamRows = $derived.by((): HealthStreamRow[] =>
		health.cameras.flatMap((camera): HealthStreamRow[] => {
			const videoStreams = camera.streams.filter((stream) => stream.type !== 'audio');
			return videoStreams.length > 0
				? videoStreams.map((stream) => ({ camera, stream }))
				: [{ camera, stream: null }];
		})
	);

	function formatBytes(bytes: number | null | undefined): string {
		if (bytes === null || bytes === undefined) return '—';
		if (bytes < 1_000) return `${bytes} B`;
		const units = ['kB', 'MB', 'GB', 'TB'];
		let value = bytes / 1_000;
		let unitIndex = 0;
		while (value >= 1_000 && unitIndex < units.length - 1) {
			value /= 1_000;
			unitIndex += 1;
		}
		return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unitIndex]}`;
	}

	function formatBitrate(bitsPerSecond: number | null | undefined): string {
		if (bitsPerSecond === null || bitsPerSecond === undefined) return '—';
		return bitsPerSecond >= 1_000_000
			? `${(bitsPerSecond / 1_000_000).toFixed(1)} Mb/s`
			: `${Math.round(bitsPerSecond / 1_000)} kb/s`;
	}

	function formatDuration(seconds: number): string {
		const days = Math.floor(seconds / 86_400);
		const hours = Math.floor((seconds % 86_400) / 3_600);
		const minutes = Math.floor((seconds % 3_600) / 60);
		if (days > 0) return `${days}d ${hours.toString().padStart(2, '0')}h`;
		if (hours > 0) return `${hours}h ${minutes.toString().padStart(2, '0')}m`;
		return `${minutes}m`;
	}

	function formatPercent(value: number | null | undefined): string {
		return value === null || value === undefined ? '—' : `${value.toFixed(1)}%`;
	}

	function formatMetric(value: number | null): string {
		return value === null ? '—' : `${value.toFixed(value >= 10 ? 0 : 1)}`;
	}

	function streamName(camera: CameraHealth, stream: StreamHealth | null): string {
		const suffix = stream?.type.replace('video_', '') ?? 'main';
		return `${camera.name} · ${suffix}`;
	}

	function streamState(camera: CameraHealth, stream: StreamHealth | null): string {
		const state = stream?.state ?? camera.state;
		return state.charAt(0).toUpperCase() + state.slice(1);
	}

	function stateColor(camera: CameraHealth, stream: StreamHealth | null): string {
		const state = stream?.state ?? camera.state;
		if (state === 'healthy') return 'bg-healthy';
		if (state === 'degraded' || state === 'stale' || state === 'reconnecting') {
			return 'bg-activity';
		}
		if (state === 'offline') return 'bg-live';
		return 'bg-text-faint';
	}

	function ratio(value: number | null, total: number): string {
		return `${value ?? '—'} / ${total}`;
	}

	function evidence(value: boolean | null | undefined): string {
		if (value === true) return 'CURRENT';
		if (value === false) return 'MISSING';
		return 'UNKNOWN';
	}

	function transportEvidence(camera: CameraHealth, stream: StreamHealth | null): boolean | null {
		return (
			stream?.dimensions?.transport_connected ?? camera.dimensions?.transport_connected ?? null
		);
	}

	function frameEvidence(camera: CameraHealth, stream: StreamHealth | null): boolean | null {
		return stream?.dimensions?.frames_fresh ?? camera.dimensions?.frames_fresh ?? null;
	}

	function decodeEvidence(camera: CameraHealth, stream: StreamHealth | null): boolean | null {
		return stream?.dimensions?.decodable ?? camera.dimensions?.decodable ?? null;
	}

	function recordingEvidence(camera: CameraHealth, stream: StreamHealth | null): string {
		const requested =
			stream?.dimensions?.recording_requested ?? camera.dimensions?.recording_requested;
		if (requested === false) return 'NOT REQUESTED';
		if (requested !== true) return 'UNKNOWN';
		return evidence(
			stream?.dimensions?.recording_progressing ?? camera.dimensions?.recording_progressing
		);
	}

	function findingHref(finding: (typeof findings)[number]): string | null {
		if (!finding.camera) return null;
		if (finding.issue.timeline_start_ms !== null && finding.issue.timeline_start_ms !== undefined) {
			const date = new Date(finding.issue.timeline_start_ms).toISOString().slice(0, 10);
			const search = new URLSearchParams({
				camera: finding.camera.id,
				date,
				at: String(finding.issue.timeline_start_ms)
			});
			return `${resolve('/keep')}?${search}`;
		}
		return `${resolve('/system-health')}/camera/${encodeURIComponent(finding.camera.id)}`;
	}
</script>

{#snippet overview()}
	<div data-health-overview-band="verdict" class="w-full {paperFrame ? 'h-[130px] shrink-0' : ''}">
		<HealthPriorityCard finding={primaryFinding} status={health.status} {paperFrame} />
	</div>

	<section
		data-health-overview-band="issues"
		class="flex w-full flex-col {paperFrame ? 'h-[246px] shrink-0' : ''}"
	>
		<header class="flex h-[38px] shrink-0 items-start justify-between pb-3">
			<h2 class="text-lg leading-[22px] font-semibold">
				<span aria-hidden="true">Open issues</span><span class="sr-only">Current findings</span>
			</h2>
			<span class="font-mono text-2xs leading-[14px] text-text-faint">
				{findings.length} SERVER FINDINGS
			</span>
		</header>
		{#each findings.slice(0, paperFrame ? 4 : findings.length) as finding, index (`${finding.issue.scope}-${finding.issue.message}-${index}`)}
			<div data-health-finding class="flex h-[52px] shrink-0 items-center border-t border-hairline">
				<span class="w-5 shrink-0"
					><span
						class="block size-[7px] rounded-full {finding.issue.severity === 'critical'
							? 'bg-live'
							: finding.issue.severity === 'warning'
								? 'bg-activity'
								: 'bg-availability'}"
					></span></span
				>
				<div class="w-[420px] shrink-0 pr-4">
					<p class="text-sm leading-[18px] font-medium">{finding.issue.message}</p>
				</div>
				<p class="w-[520px] shrink-0 pr-4 text-[13px] leading-4 text-text-muted">
					{finding.camera?.last_error ?? `Server-authored finding · ${finding.issue.scope}`}
				</p>
				<span class="w-[180px] shrink-0 font-mono text-xs-plus text-text-muted">
					{finding.camera ? streamState(finding.camera, null) : finding.issue.severity}
				</span>
				{#if finding.camera && findingHref(finding)}
					<a
						href={findingHref(finding) ?? undefined}
						class="w-[170px] shrink-0 text-right text-[13px] text-primary-soft"
						aria-label={finding.issue.operational_event_id
							? `Open ${finding.camera.name} outage in timeline`
							: `Diagnose ${finding.camera.name} from findings`}
						>{finding.issue.operational_event_id ? 'Open timeline' : 'Diagnose'}</a
					>
				{/if}
			</div>
		{/each}
		{#if paperFrame && findings.length < 4}
			{#each Array.from({ length: 4 - findings.length }) as _, index (index)}
				<div
					class="flex h-[52px] shrink-0 items-center border-t border-hairline text-[13px] text-text-faint"
				>
					<span class="w-5 shrink-0"
						><span class="block size-[7px] rounded-full bg-availability"></span></span
					>
					No additional server finding
				</div>
			{/each}
		{/if}
	</section>

	<section
		data-health-overview-band="stats"
		class="flex w-full overflow-hidden rounded-md border border-hairline bg-hairline {paperFrame
			? 'h-[130px] shrink-0 gap-px'
			: 'min-h-[130px] flex-wrap gap-px'}"
		aria-label="Health summary"
	>
		{#each [['CONFIGURED', `${fleetCounts.configured}`, `${health.totals.configured_video_streams} expected video streams`], ['CONNECTED', ratio(fleetCounts.connected, fleetCounts.configured), `${health.totals.connected_video_streams ?? '—'} transports connected`], ['FRESH', ratio(fleetCounts.fresh, fleetCounts.configured), `${health.totals.fresh_video_streams ?? '—'} streams with current frames`], ['DECODABLE', ratio(fleetCounts.decodable, fleetCounts.configured), `${health.totals.decodable_video_streams ?? '—'} streams with recent keyframes`], ['RECORDING', ratio(fleetCounts.recording, health.totals.recording_requested_cameras ?? 0), `${health.totals.recording_video_streams ?? '—'} of ${health.totals.recording_requested_video_streams ?? '—'} requested writers progressing`]] as stat (stat[0])}
			<div
				data-health-metric={stat[0] === 'CPU'
					? 'Process CPU'
					: stat[0] === 'MEMORY'
						? 'Process memory'
						: stat[0]}
				class="flex h-32 w-[261px] shrink-0 flex-col gap-2 bg-surface p-[18px]"
			>
				<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">{stat[0]}</p>
				<p class="text-[28px] leading-[34px] font-semibold">{stat[1]}</p>
				<span class="h-1 w-[225px] rounded-full bg-hairline"></span>
				<p class="text-xs-plus leading-4 text-text-muted">{stat[2]}</p>
			</div>
		{/each}
	</section>

	<section
		data-health-overview-band="streams"
		class="flex w-full flex-col {paperFrame ? 'h-[248px] shrink-0 overflow-hidden' : ''}"
		aria-labelledby="streams-heading"
	>
		<header class="flex h-[34px] shrink-0 items-start justify-between pb-3">
			<div>
				<h2 id="streams-heading" class="text-sm font-semibold">Camera streams</h2>
				<p class="text-[11px] text-text-muted">
					{health.totals.connected_cameras ?? '—'} connected · {health.totals.fresh_cameras ?? '—'} fresh
					·
					{health.totals.decodable_cameras ?? '—'} decodable · {health.totals.recording_cameras ??
						'—'} recording
				</p>
			</div>
			<p class="font-mono text-[10px] text-text-faint">
				{compact(health.totals.frames)} frames · {compact(health.totals.keyframes)} keyframes · {compact(
					health.totals.drops
				)} drops · {compact(health.totals.errors)} errors · {compact(health.totals.reconnects)} reconnects
			</p>
		</header>
		<div
			class="flex h-[30px] shrink-0 items-center border-b border-hairline-strong font-mono text-2xs tracking-[0.14em] text-text-faint"
		>
			<span class="w-[220px]">STREAM</span><span class="w-[230px]">STATE / REASON</span><span
				class="w-[145px]">TRANSPORT</span
			><span class="w-[150px]">FRAMES</span><span class="w-[150px]">DECODABLE</span><span
				class="w-[170px]">RECORDING</span
			><span class="w-[145px]">LAST REPORT</span><span class="w-[100px]">FORMAT</span>
		</div>
		{#each streamRows.slice(0, paperFrame ? 4 : streamRows.length) as row (`${row.camera.id}-${row.stream?.type ?? 'none'}`)}
			<div
				data-health-stream-row
				class="flex h-[46px] shrink-0 items-center border-b border-hairline text-[13px]"
			>
				<span class="w-[220px] shrink-0 text-sm">{streamName(row.camera, row.stream)}</span>
				<span class="flex w-[230px] shrink-0 items-center gap-2 pr-3"
					><span class="size-1.5 shrink-0 rounded-full {stateColor(row.camera, row.stream)}"
					></span><span class="min-w-0"
						><span class="block">{streamState(row.camera, row.stream)}</span><span
							class="block truncate text-2xs text-text-faint"
							>{row.stream?.detail ??
								row.camera.detail ??
								row.camera.reason ??
								'Evidence unavailable'}</span
						></span
					></span
				>
				<span class="w-[145px] shrink-0 font-mono"
					>{evidence(transportEvidence(row.camera, row.stream))}</span
				>
				<span class="w-[150px] shrink-0 font-mono"
					>{evidence(frameEvidence(row.camera, row.stream))}</span
				>
				<span class="w-[150px] shrink-0 font-mono"
					>{evidence(decodeEvidence(row.camera, row.stream))}</span
				>
				<span class="w-[170px] shrink-0 font-mono">{recordingEvidence(row.camera, row.stream)}</span
				>
				<span class="w-[145px] shrink-0 font-mono"
					>{row.stream ? `${Math.round(row.stream.report_age_ms / 1_000)}s ago` : '—'}</span
				>
				<span class="w-[100px] shrink-0 truncate font-mono text-xs text-text-muted"
					>{row.stream
						? `${row.stream.codec ?? '—'} · ${row.stream.resolution ?? '—'}`
						: (row.camera.lifecycle ?? 'No stream report')}</span
				>
			</div>
		{/each}
	</section>

	<section
		data-health-overview-band="client"
		class="flex w-full items-start gap-5 {paperFrame ? 'h-[326px] shrink-0' : 'flex-wrap'}"
	>
		<article
			class="flex h-[326px] w-[645px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-[18px]"
		>
			<div class="flex flex-col gap-1">
				<h2 class="text-lg leading-[22px] font-semibold">This browser</h2>
				<p class="text-[13px] leading-[21px] text-text-muted">
					Browser receiver evidence distinguishes client playback from server ingest and storage.
				</p>
			</div>
			<div
				class="flex h-[66px] shrink-0 gap-px overflow-hidden rounded-sm border border-hairline bg-hairline"
			>
				{#each [['ROUND TRIP', browser.roundTripMs === null ? '—' : `${browser.roundTripMs} ms`], ['JITTER', browser.jitterMs === null ? '—' : `${browser.jitterMs} ms`], ['PACKET LOSS', browser.packetLossPercent === null ? '—' : `${browser.packetLossPercent.toFixed(1)}%`], ['FRAMES DROPPED', browser.framesDropped ?? '—']] as metric (metric[0])}
					<div class="flex w-[151px] shrink-0 flex-col gap-1 bg-raised p-3">
						<span class="font-mono text-[10px] leading-3 tracking-[0.1em] text-text-faint"
							>{metric[0]}</span
						>
						<span class="font-mono text-lg leading-[22px]">{metric[1]}</span>
					</div>
				{/each}
			</div>
			<dl class="flex flex-col">
				{#each [['Connection', browser.connection], ['Decoder', browser.decoder], ['Presented', browser.presented], ['Quality in use', browser.quality]] as row (row[0])}
					<div
						class="flex h-8 shrink-0 items-center justify-between border-b border-hairline text-[13px]"
					>
						<dt class="text-text-muted">{row[0]}</dt>
						<dd class="font-mono text-xs">{row[1]}</dd>
					</div>
				{/each}
			</dl>
		</article>

		<article
			class="flex h-[326px] w-[645px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-[18px]"
		>
			<div class="flex flex-col gap-1">
				<h2 class="text-lg leading-[22px] font-semibold">Machine-readable evidence</h2>
				<p class="text-[13px] leading-[21px] text-text-muted">
					The browser reads the same canonical transports available to other clients.
				</p>
			</div>
			<div class="flex flex-col gap-2.5">
				{#each [['HealthCommand', 'WebRTC · protobuf snapshot'], ['/metrics', 'HTTP · Prometheus text'], ['/logs', 'HTTP · redacted live tail']] as endpoint (endpoint[0])}
					<div class="flex h-[66px] shrink-0 items-center justify-between rounded-sm bg-raised p-3">
						<div>
							<p class="font-mono text-[13px]">{endpoint[0]}</p>
							<p class="mt-0.5 text-xs text-text-muted">{endpoint[1]}</p>
						</div>
						<span class="flex items-center gap-1.5 font-mono text-2xs text-healthy"
							><span class="size-1.5 rounded-full bg-healthy"></span>AVAILABLE</span
						>
					</div>
				{/each}
			</div>
		</article>
	</section>
{/snippet}

{#if paperFrame}
	<section
		data-desktop-health-overview
		class="flex h-[1302px] w-[1440px] overflow-hidden rounded-lg border border-hairline bg-surface [font-synthesis:none]"
	>
		<DesktopPaperRail active="health" />
		<div class="flex h-[1300px] w-[1374px] shrink-0 flex-col">
			<header
				class="flex h-[52px] shrink-0 items-center justify-between border-b border-hairline px-5"
			>
				<div class="flex items-baseline gap-3">
					<h1 class="text-base font-semibold">Health</h1>
					<span class="font-mono text-2xs text-text-muted"
						>UPTIME {formatDuration(health.uptime_seconds)} · v{health.version}</span
					>
				</div>
				<div class="flex gap-2.5">
					<button
						type="button"
						class="h-[30px] rounded-sm border border-hairline-strong px-3 text-[13px] text-text-muted"
						disabled>Diagnostics bundle unavailable</button
					><a
						href={resolve('/settings/logs')}
						class="inline-flex h-[30px] items-center rounded-sm border border-hairline-strong px-3 text-[13px] text-text-muted"
						>Open live logs</a
					>
				</div>
			</header>
			<div class="flex h-[1248px] shrink-0 flex-col gap-7 px-8 py-7">
				{@render overview()}
			</div>
		</div>
	</section>
{:else}
	<div data-desktop-health-overview class="flex flex-col gap-6">
		{@render overview()}
	</div>
{/if}
