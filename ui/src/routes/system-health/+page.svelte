<script lang="ts">
	import { resolve } from '$app/paths';
	import { untrack } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import { getServerHealth } from '$lib/api';
	import { useLivePeer } from '$lib/stream-peer-context';
	import type { LivePeerTrack } from '$lib/stream-peer.svelte';
	import type {
		CameraHealth,
		DiskHealth,
		HealthIssue,
		ServerHealthResponse,
		StreamHealth
	} from '$lib/types';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import AlertTriangleIcon from '@lucide/svelte/icons/triangle-alert';
	import CheckCircleIcon from '@lucide/svelte/icons/circle-check';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import CpuIcon from '@lucide/svelte/icons/cpu';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import HardDriveIcon from '@lucide/svelte/icons/hard-drive';
	import MemoryStickIcon from '@lucide/svelte/icons/memory-stick';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import ServerIcon from '@lucide/svelte/icons/server';
	import ThermometerIcon from '@lucide/svelte/icons/thermometer';
	import UploadIcon from '@lucide/svelte/icons/upload';

	const REFRESH_INTERVAL_MS = 5_000;
	const CLIENT_STATS_INTERVAL_MS = 1_000;
	const numberFormatter = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
	const compactFormatter = new Intl.NumberFormat(undefined, {
		notation: 'compact',
		maximumFractionDigits: 1
	});
	type ExtendedInboundStats = RTCInboundRtpStreamStats & {
		decoderImplementation?: string;
	};
	type ClientStatsSample = {
		ssrc: number;
		timestamp: number;
		bytesReceived: number;
		framesReceived: number;
	};
	type ClientReceiverHealth = {
		codec: string | null;
		resolution: string | null;
		framesPerSecond: number | null;
		receiveBitrateBps: number | null;
		packetsReceived: number;
		packetsLost: number;
		packetLossPercent: number | null;
		jitterMs: number | null;
		roundTripTimeMs: number | null;
		framesDecoded: number;
		framesDropped: number;
		decoderImplementation: string | null;
	};

	let health = $state.raw<ServerHealthResponse | null>(null);
	let loading = $state(true);
	let refreshing = $state(false);
	let error: string | null = $state(null);
	let activeTab = $state<'client' | 'server'>('server');
	let clientReceiverHealth = $state.raw<Record<string, ClientReceiverHealth>>({});
	let requestVersion = 0;
	let clientStatsRefreshInFlight = false;
	const clientStatsSamples = new SvelteMap<string, ClientStatsSample>();
	const livePeer = useLivePeer();

	let recordingDisk = $derived(health?.system.disks.find((disk) => disk.stores_recordings) ?? null);
	let memoryUsedPercent = $derived(
		health && health.system.memory.total_bytes > 0
			? (health.system.memory.used_bytes / health.system.memory.total_bytes) * 100
			: 0
	);
	let audioStreams = $derived(
		(health?.cameras ?? []).flatMap((camera) =>
			camera.streams
				.filter((stream) => stream.type === 'audio')
				.map((stream) => ({ camera, stream }))
		)
	);
	let clientTracks = $derived(Object.values(livePeer.tracks));
	let clientQueues = $derived(
		health && livePeer.sessionId !== null
			? health.webrtc.session_queues.filter((queue) => queue.session_id === livePeer.sessionId)
			: []
	);
	let clientMainTracks = $derived(
		clientTracks.filter((track) => track.activeStream === 'main').length
	);
	let clientSubTracks = $derived(
		clientTracks.filter((track) => track.activeStream === 'sub').length
	);
	$effect(() => {
		const sessionId = livePeer.sessionId;
		const receiverKey = clientTracks
			.map((track) => `${track.trackId}:${track.receiver?.track?.id ?? ''}`)
			.join('|');
		if (activeTab !== 'client' || sessionId === null || receiverKey.length === 0) {
			clientReceiverHealth = {};
			clientStatsSamples.clear();
			return;
		}

		void refreshClientReceiverHealth(sessionId);
		const timer = window.setInterval(
			() => void refreshClientReceiverHealth(sessionId),
			CLIENT_STATS_INTERVAL_MS
		);
		return () => window.clearInterval(timer);
	});
	$effect.pre(() => {
		untrack(() => void loadHealth());
		const timer = window.setInterval(() => {
			if (document.visibilityState === 'visible') void loadHealth();
		}, REFRESH_INTERVAL_MS);
		return () => window.clearInterval(timer);
	});

	async function loadHealth() {
		const version = ++requestVersion;
		refreshing = health !== null;
		try {
			const next = await getServerHealth();
			if (version !== requestVersion) return;
			health = next;
			error = null;
		} catch (cause) {
			if (version !== requestVersion) return;
			error = cause instanceof Error ? cause.message : 'Health snapshot is unavailable';
		} finally {
			if (version === requestVersion) {
				loading = false;
				refreshing = false;
			}
		}
	}

	async function refreshClientReceiverHealth(expectedSessionId: number) {
		if (clientStatsRefreshInFlight) return;
		clientStatsRefreshInFlight = true;
		try {
			const entries = await Promise.all(
				clientTracks.map(async (track) => {
					const stats = await readClientReceiverHealth(track);
					return stats ? ([track.trackId, stats] as const) : null;
				})
			);
			if (livePeer.sessionId !== expectedSessionId) return;
			clientReceiverHealth = Object.fromEntries(entries.filter((entry) => entry !== null));
		} finally {
			clientStatsRefreshInFlight = false;
		}
	}

	async function readClientReceiverHealth(
		track: LivePeerTrack
	): Promise<ClientReceiverHealth | null> {
		if (!track.receiver) return null;
		const reports = await track.receiver.getStats();
		const inbound = [...reports.values()].find(
			(report) => report.type === 'inbound-rtp' && report.kind === 'video'
		) as ExtendedInboundStats | undefined;
		if (!inbound) return null;

		const sample: ClientStatsSample = {
			ssrc: inbound.ssrc,
			timestamp: inbound.timestamp,
			bytesReceived: (inbound.bytesReceived ?? 0) + (inbound.headerBytesReceived ?? 0),
			framesReceived: inbound.framesReceived ?? 0
		};
		const previous = clientStatsSamples.get(track.trackId);
		let receiveBitrateBps: number | null = null;
		let measuredFramesPerSecond: number | null = null;
		if (previous?.ssrc === sample.ssrc && sample.timestamp > previous.timestamp) {
			const elapsedSeconds = (sample.timestamp - previous.timestamp) / 1_000;
			receiveBitrateBps =
				(Math.max(0, sample.bytesReceived - previous.bytesReceived) * 8) / elapsedSeconds;
			measuredFramesPerSecond =
				Math.max(0, sample.framesReceived - previous.framesReceived) / elapsedSeconds;
		}
		clientStatsSamples.set(track.trackId, sample);

		const packetsReceived = inbound.packetsReceived ?? 0;
		const packetsLost = Math.max(0, inbound.packetsLost ?? 0);
		const totalPackets = packetsReceived + packetsLost;
		const codec = inbound.codecId
			? (reports.get(inbound.codecId) as { type: string; mimeType?: string } | undefined)
			: undefined;
		const transport = inbound.transportId
			? (reports.get(inbound.transportId) as RTCTransportStats | undefined)
			: undefined;
		const candidatePair = transport?.selectedCandidatePairId
			? (reports.get(transport.selectedCandidatePairId) as RTCIceCandidatePairStats | undefined)
			: undefined;
		return {
			codec: codec?.type === 'codec' ? (codec.mimeType?.replace(/^video\//i, '') ?? null) : null,
			resolution:
				inbound.frameWidth && inbound.frameHeight
					? `${inbound.frameWidth} × ${inbound.frameHeight}`
					: null,
			framesPerSecond:
				inbound.framesPerSecond && inbound.framesPerSecond > 0
					? inbound.framesPerSecond
					: measuredFramesPerSecond,
			receiveBitrateBps,
			packetsReceived,
			packetsLost,
			packetLossPercent: totalPackets > 0 ? (packetsLost / totalPackets) * 100 : null,
			jitterMs: inbound.jitter === undefined ? null : inbound.jitter * 1_000,
			roundTripTimeMs:
				candidatePair?.currentRoundTripTime === undefined
					? null
					: candidatePair.currentRoundTripTime * 1_000,
			framesDecoded: inbound.framesDecoded ?? 0,
			framesDropped: inbound.framesDropped ?? 0,
			decoderImplementation: inbound.decoderImplementation ?? null
		};
	}

	function formatBytes(bytes: number | null | undefined): string {
		if (bytes === null || bytes === undefined) return '—';
		if (bytes < 1_000) return `${bytes} B`;
		const units = ['kB', 'MB', 'GB', 'TB', 'PB'];
		let value = bytes / 1_000;
		let index = 0;
		while (value >= 1_000 && index < units.length - 1) {
			value /= 1_000;
			index += 1;
		}
		return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[index]}`;
	}

	function formatBitrate(bitsPerSecond: number | null | undefined): string {
		if (bitsPerSecond === null || bitsPerSecond === undefined) return '—';
		if (bitsPerSecond >= 1_000_000_000) return `${(bitsPerSecond / 1_000_000_000).toFixed(2)} Gbps`;
		if (bitsPerSecond >= 1_000_000) return `${(bitsPerSecond / 1_000_000).toFixed(1)} Mbps`;
		if (bitsPerSecond >= 1_000) return `${Math.round(bitsPerSecond / 1_000)} kbps`;
		return `${Math.round(bitsPerSecond)} bps`;
	}

	function formatMegabits(bitsPerSecond: number): string {
		const megabits = bitsPerSecond / 1_000_000;
		return `${megabits.toFixed(megabits >= 10 ? 1 : 2)} Mbps`;
	}

	function formatFrameSize(kilobytes: number | null | undefined): string {
		return kilobytes && kilobytes > 0 ? formatBytes(kilobytes * 1_000) : '—';
	}

	function formatPercent(value: number | null | undefined): string {
		return value === null || value === undefined ? '—' : `${value.toFixed(1)}%`;
	}

	function formatCpuCores(value: number | null | undefined): string {
		if (value === null || value === undefined) return '—';
		return `${value.toFixed(value >= 10 ? 1 : 2)} cores`;
	}

	function formatDuration(seconds: number | null | undefined): string {
		if (seconds === null || seconds === undefined) return '—';
		const days = Math.floor(seconds / 86_400);
		const hours = Math.floor((seconds % 86_400) / 3_600);
		const minutes = Math.floor((seconds % 3_600) / 60);
		if (days > 0) return `${days}d ${hours}h`;
		if (hours > 0) return `${hours}h ${minutes}m`;
		return `${minutes}m ${Math.floor(seconds % 60)}s`;
	}

	function formatAge(milliseconds: number): string {
		if (milliseconds < 1_000) return 'now';
		if (milliseconds < 60_000) return `${Math.round(milliseconds / 1_000)}s`;
		return `${Math.round(milliseconds / 60_000)}m`;
	}

	function formatMilliseconds(value: number | null | undefined): string {
		return value === null || value === undefined ? '—' : `${numberFormatter.format(value)} ms`;
	}

	function formatPacketLoss(stats: ClientReceiverHealth | undefined): string {
		if (!stats) return '—';
		const percent = stats.packetLossPercent;
		return percent === null
			? compactFormatter.format(stats.packetsLost)
			: `${compactFormatter.format(stats.packetsLost)} (${percent.toFixed(2)}%)`;
	}

	function formatTemperature(value: number | null): string {
		return value === null ? '—' : `${value.toFixed(1)} °C`;
	}

	function streamLabel(stream: StreamHealth): string {
		if (stream.type === 'video_main') return 'Main';
		if (stream.type === 'video_sub') return 'Sub';
		return stream.type.replaceAll('_', ' ');
	}

	function streamUtilization(stream: StreamHealth): number {
		if (!stream.expected_fps) return 0;
		return Math.max(0, Math.min(100, ((stream.fps ?? 0) / stream.expected_fps) * 100));
	}

	function severityClasses(issue: HealthIssue): string {
		if (issue.severity === 'critical')
			return 'border-red-500/30 bg-red-500/8 text-red-700 dark:text-red-300';
		if (issue.severity === 'warning')
			return 'border-amber-500/30 bg-amber-500/8 text-amber-800 dark:text-amber-300';
		return 'border-sky-500/25 bg-sky-500/7 text-sky-800 dark:text-sky-300';
	}

	function stateColor(state: CameraHealth['state']): string {
		if (state === 'online') return 'bg-emerald-500';
		if (state === 'starting') return 'bg-sky-500';
		if (state === 'degraded' || state === 'stale') return 'bg-amber-500';
		return 'bg-red-500';
	}

	function diskFreePercent(disk: DiskHealth | null): number {
		return disk && disk.total_bytes > 0 ? (disk.available_bytes / disk.total_bytes) * 100 : 0;
	}

	function cameraHref(camera: CameraHealth): string {
		return `${resolve('/camera')}?camera=${encodeURIComponent(camera.id)}`;
	}

	function cameraName(cameraId: string): string {
		return health?.cameras.find((camera) => camera.id === cameraId)?.name ?? cameraId;
	}
</script>

<svelte:head>
	<title>Health - KeepPeek</title>
</svelte:head>

<div class="mx-auto max-w-[120rem] space-y-6">
	<header class="flex flex-wrap items-start gap-3">
		<div class="min-w-0">
			<h1 class="text-xl font-semibold">Health</h1>
			{#if health}
				<p class="mt-0.5 truncate text-xs text-muted-foreground">
					{health.system.host_name ?? 'Local host'} · {health.system.os_version ??
						health.system.os_name ??
						'Unknown OS'} · v{health.version}
				</p>
			{/if}
		</div>
		<div class="ml-auto flex items-center gap-2">
			{#if health}
				<span class="text-[11px] text-muted-foreground">
					Updated {new Date(health.generated_at_ms).toLocaleTimeString()}
				</span>
			{/if}
			<button
				type="button"
				class="grid size-8 place-items-center rounded-md border bg-background text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				title="Refresh health snapshot"
				aria-label="Refresh health snapshot"
				disabled={refreshing}
				onclick={() => void loadHealth()}
			>
				<RefreshCwIcon class="size-3.5 {refreshing ? 'animate-spin' : ''}" />
			</button>
		</div>
	</header>

	{#if loading && !health}
		<div class="grid min-h-72 place-items-center border-y text-sm text-muted-foreground">
			Loading server health
		</div>
	{:else if error && !health}
		<div
			class="border-y border-destructive/40 bg-destructive/8 px-4 py-3 text-sm text-destructive"
			role="alert"
		>
			{error}
		</div>
	{:else if health}
		{#if error}
			<div
				class="border-y border-amber-500/30 bg-amber-500/8 px-4 py-2 text-xs text-amber-800 dark:text-amber-300"
				role="status"
			>
				Latest refresh failed: {error}
			</div>
		{/if}

		<div class="flex border-b" role="tablist" aria-label="Health scope">
			<button
				id="client-health-tab"
				type="button"
				role="tab"
				aria-selected={activeTab === 'client'}
				aria-controls="client-health-panel"
				class="border-b-2 px-4 py-2 text-sm font-medium {activeTab === 'client'
					? 'border-primary text-foreground'
					: 'border-transparent text-muted-foreground hover:text-foreground'}"
				onclick={() => (activeTab = 'client')}
			>
				Client
			</button>
			<button
				id="server-health-tab"
				type="button"
				role="tab"
				aria-selected={activeTab === 'server'}
				aria-controls="server-health-panel"
				class="border-b-2 px-4 py-2 text-sm font-medium {activeTab === 'server'
					? 'border-primary text-foreground'
					: 'border-transparent text-muted-foreground hover:text-foreground'}"
				onclick={() => (activeTab = 'server')}
			>
				Server
			</button>
		</div>

		{#if activeTab === 'client'}
			<div
				id="client-health-panel"
				role="tabpanel"
				aria-labelledby="client-health-tab"
				class="space-y-6"
			>
				<section aria-labelledby="client-connection-heading">
					<div class="mb-3 flex items-center gap-2">
						<RadioIcon class="size-4 text-emerald-600" />
						<h2 id="client-connection-heading" class="text-sm font-semibold">Current client</h2>
					</div>
					<div class="grid grid-cols-2 divide-x divide-y border-y sm:grid-cols-4 xl:grid-cols-7">
						{#each [['Session', livePeer.sessionId === null ? '—' : `#${livePeer.sessionId}`], ['Connection', livePeer.connectionState], ['ICE', livePeer.iceConnectionState], ['Tracks', clientTracks.length], ['Main', clientMainTracks], ['Sub', clientSubTracks], ['BWE avg', formatBitrate(livePeer.estimatedBitrateBps)]] as metric (metric[0])}
							<div class="min-w-0 p-3" data-health-metric={metric[0]}>
								<p class="text-[9px] font-semibold text-muted-foreground uppercase">{metric[0]}</p>
								<p class="mt-1 truncate font-mono text-sm font-semibold capitalize">{metric[1]}</p>
							</div>
						{/each}
					</div>
				</section>

				{#if livePeer.sessionId === null || clientTracks.length === 0}
					<div class="grid min-h-48 place-items-center border-y px-4 text-center" role="status">
						<div>
							<p class="text-sm font-medium">No active client streams</p>
							<p class="mt-1 text-xs text-muted-foreground">
								Open Health from an active Peek view to inspect that browser session.
							</p>
						</div>
					</div>
				{:else}
					<section aria-labelledby="client-streams-heading">
						<div class="mb-2">
							<h2 id="client-streams-heading" class="text-sm font-semibold">Client streams</h2>
							<p class="text-[11px] text-muted-foreground">
								Browser receiver and matching server delivery queue for each active track
							</p>
						</div>
						<div class="overflow-x-auto border-y">
							<table class="w-full min-w-[96rem] text-left text-[11px]">
								<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase">
									<tr>
										<th class="px-3 py-2">Camera</th><th class="px-3 py-2">Track</th><th
											class="px-3 py-2">Status</th
										><th class="px-3 py-2">Quality</th><th class="px-3 py-2">Format</th><th
											class="px-3 py-2">Receive</th
										><th class="px-3 py-2">Packet loss</th><th class="px-3 py-2">Jitter</th><th
											class="px-3 py-2">RTT</th
										><th class="px-3 py-2">Decode</th><th class="px-3 py-2">Server queue</th><th
											class="px-3 py-2">Queue loss</th
										>
									</tr>
								</thead>
								<tbody class="divide-y">
									{#each clientTracks as track (track.trackId)}
										{@const queue = clientQueues.find((entry) => entry.track_id === track.trackId)}
										{@const receiverStats = clientReceiverHealth[track.trackId]}
										<tr>
											<td class="px-3 py-2 font-medium">{cameraName(track.cameraId)}</td><td
												class="px-3 py-2 font-mono">{track.trackId}</td
											><td class="px-3 py-2 capitalize">{track.status}</td><td
												class="px-3 py-2 capitalize"
												>{track.requestedQuality} / {track.activeStream}</td
											><td class="px-3 py-2 font-mono uppercase"
												>{receiverStats?.codec ?? '—'}
												<p class="text-[9px] text-muted-foreground">
													{receiverStats?.resolution ?? '—'}
												</p></td
											><td class="px-3 py-2 font-mono"
												>{formatBitrate(receiverStats?.receiveBitrateBps)}
												<p class="text-[9px] text-muted-foreground">
													{receiverStats?.framesPerSecond === null ||
													receiverStats?.framesPerSecond === undefined
														? '—'
														: `${numberFormatter.format(receiverStats.framesPerSecond)} fps`}
												</p>
												<p class="text-[9px] text-muted-foreground">
													BWE {formatBitrate(track.estimatedBitrateBps)}
												</p></td
											><td class="px-3 py-2 font-mono"
												>{formatPacketLoss(receiverStats)}
												<p class="text-[9px] text-muted-foreground">
													{receiverStats
														? `${compactFormatter.format(receiverStats.packetsReceived)} received`
														: '—'}
												</p></td
											><td class="px-3 py-2 font-mono"
												>{formatMilliseconds(receiverStats?.jitterMs)}</td
											><td class="px-3 py-2 font-mono"
												>{formatMilliseconds(receiverStats?.roundTripTimeMs)}</td
											><td class="px-3 py-2 font-mono"
												>{receiverStats
													? compactFormatter.format(receiverStats.framesDecoded)
													: '—'}
												<p class="text-[9px] text-muted-foreground">
													{receiverStats
														? `${compactFormatter.format(receiverStats.framesDropped)} dropped`
														: '—'}
												</p>
												<p class="max-w-36 truncate text-[9px] text-muted-foreground">
													{receiverStats?.decoderImplementation ?? 'Browser decoder'}
												</p></td
											><td class="px-3 py-2 font-mono"
												>{queue ? `${queue.depth} / ${health.webrtc.queue_capacity}` : '—'}
												<p class="text-[9px] text-muted-foreground">
													{queue ? `peak ${queue.high_water}` : '—'}
												</p></td
											><td class="px-3 py-2 font-mono"
												>{queue
													? queue.full_drops + queue.discarded_frames + queue.recovery_drops
													: '—'}
												<p class="text-[9px] text-muted-foreground">
													{queue
														? `${queue.full_drops} full · ${queue.discarded_frames} discarded · ${queue.recovery_drops} recovery`
														: '—'}
												</p>
												<p class="text-[9px] text-muted-foreground">
													{queue ? `${compactFormatter.format(queue.written_frames)} written` : '—'}
												</p></td
											>
										</tr>
									{/each}
								</tbody>
							</table>
						</div>
					</section>
				{/if}
			</div>
		{:else}
			<div
				id="server-health-panel"
				role="tabpanel"
				aria-labelledby="server-health-tab"
				class="space-y-6"
			>
				<section
					class="grid grid-cols-2 divide-x divide-y border-y md:grid-cols-3 xl:grid-cols-7"
					aria-label="Health summary"
				>
					<div class="min-w-0 p-3">
						<div
							class="flex items-center gap-1.5 text-[10px] font-semibold text-muted-foreground uppercase"
						>
							{#if health.status === 'healthy'}
								<CheckCircleIcon class="size-3.5 text-emerald-500" />
							{:else}
								<AlertTriangleIcon class="size-3.5 text-amber-500" />
							{/if}
							Server
						</div>
						<p class="mt-1 text-lg font-semibold capitalize">{health.status}</p>
						<p class="text-[11px] text-muted-foreground">
							Up {formatDuration(health.uptime_seconds)}
						</p>
					</div>
					<div class="min-w-0 p-3" data-health-metric="Server egress">
						<div
							class="flex items-center gap-1.5 text-[10px] font-semibold text-muted-foreground uppercase"
						>
							<UploadIcon class="size-3.5" />Server egress
						</div>
						<p class="mt-1 text-lg font-semibold tabular-nums">
							{formatMegabits(health.system.network_egress_bps)}
						</p>
						<p class="text-[11px] text-muted-foreground">Non-loopback host traffic</p>
					</div>
					<div class="min-w-0 p-3" data-health-metric="Process CPU">
						<div
							class="flex items-center gap-1.5 text-[10px] font-semibold text-muted-foreground uppercase"
						>
							<CpuIcon class="size-3.5" />Process CPU
						</div>
						<p class="mt-1 text-lg font-semibold tabular-nums">
							{formatPercent(health.system.process.cpu_capacity_percent)}
						</p>
						<p class="text-[11px] text-muted-foreground">
							{formatCpuCores(health.system.process.cpu_core_equivalents)} · host {formatPercent(
								health.system.system_cpu_percent
							)}
						</p>
					</div>
					<div class="min-w-0 p-3" data-health-metric="Process memory">
						<div
							class="flex items-center gap-1.5 text-[10px] font-semibold text-muted-foreground uppercase"
						>
							<MemoryStickIcon class="size-3.5" />Process memory
						</div>
						<p class="mt-1 text-lg font-semibold tabular-nums">
							{formatBytes(health.system.process.resident_memory_bytes)}
						</p>
						<p class="text-[11px] text-muted-foreground">
							{formatPercent(health.system.process.memory_capacity_percent)} of {formatBytes(
								health.system.memory.total_bytes
							)} RAM
						</p>
					</div>
					<div class="min-w-0 p-3">
						<div
							class="flex items-center gap-1.5 text-[10px] font-semibold text-muted-foreground uppercase"
						>
							<ActivityIcon class="size-3.5" />Camera ingress
						</div>
						<p class="mt-1 text-lg font-semibold tabular-nums">
							{formatBitrate(health.totals.ingress_bitrate_bps)}
						</p>
						<p class="text-[11px] text-muted-foreground">
							{numberFormatter.format(health.totals.ingress_fps)} aggregate FPS
						</p>
					</div>
					<div class="min-w-0 p-3">
						<div
							class="flex items-center gap-1.5 text-[10px] font-semibold text-muted-foreground uppercase"
						>
							<RadioIcon class="size-3.5" />WebRTC
						</div>
						<p class="mt-1 text-lg font-semibold tabular-nums">{health.webrtc.active_sessions}</p>
						<p class="text-[11px] text-muted-foreground">
							{health.webrtc.active_main} main · {health.webrtc.active_sub} sub
						</p>
					</div>
					<div class="min-w-0 p-3">
						<div
							class="flex items-center gap-1.5 text-[10px] font-semibold text-muted-foreground uppercase"
						>
							<HardDriveIcon class="size-3.5" />Recording disk
						</div>
						<p class="mt-1 text-lg font-semibold tabular-nums">
							{recordingDisk ? formatBytes(recordingDisk.available_bytes) : '—'}
						</p>
						<p class="text-[11px] text-muted-foreground">
							{recordingDisk
								? `${diskFreePercent(recordingDisk).toFixed(1)}% free`
								: 'Disk unavailable'}
						</p>
					</div>
				</section>

				{#if health.issues.length > 0}
					<section aria-labelledby="issues-heading">
						<div class="mb-2 flex items-center justify-between gap-3">
							<h2 id="issues-heading" class="text-sm font-semibold">Current findings</h2>
							<span class="text-xs text-muted-foreground">{health.issues.length}</span>
						</div>
						<div class="divide-y border-y">
							{#each health.issues as issue, index (`${issue.scope}-${issue.message}-${index}`)}
								<div
									class="flex items-start gap-3 border-l-2 px-3 py-2.5 text-xs {severityClasses(
										issue
									)}"
								>
									<AlertTriangleIcon class="mt-0.5 size-3.5 shrink-0" />
									<div class="min-w-0">
										<p class="font-semibold capitalize">{issue.scope}</p>
										<p class="mt-0.5 opacity-85">{issue.message}</p>
									</div>
									<span class="ml-auto shrink-0 font-mono text-[9px] uppercase opacity-60"
										>{issue.severity}</span
									>
								</div>
							{/each}
						</div>
					</section>
				{/if}

				<section aria-labelledby="streams-heading">
					<div class="mb-2 flex flex-wrap items-end justify-between gap-2">
						<div>
							<h2 id="streams-heading" class="text-sm font-semibold">Camera streams</h2>
							<p class="text-[11px] text-muted-foreground">
								{health.totals.reporting_cameras} of {health.totals.configured_cameras} cameras ·
								{health.totals.reporting_video_streams} of {health.totals.configured_video_streams} streams
								reporting
							</p>
						</div>
						<div class="flex gap-3 font-mono text-[10px] text-muted-foreground">
							<span>{compactFormatter.format(health.totals.frames)} frames</span>
							<span>{compactFormatter.format(health.totals.keyframes)} keyframes</span>
							<span>{compactFormatter.format(health.totals.drops)} drops</span>
							<span>{compactFormatter.format(health.totals.errors)} errors</span>
							<span>{compactFormatter.format(health.totals.reconnects)} reconnects</span>
						</div>
					</div>
					<div class="overflow-x-auto border-y">
						<table class="w-full min-w-[82rem] text-left text-[11px]">
							<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase">
								<tr>
									<th class="px-3 py-2 font-semibold">Camera</th>
									<th class="px-3 py-2 font-semibold">Stream</th>
									<th class="px-3 py-2 font-semibold">Transport</th>
									<th class="px-3 py-2 font-semibold">Format</th>
									<th class="px-3 py-2 font-semibold">FPS</th>
									<th class="px-3 py-2 font-semibold">Bitrate</th>
									<th class="px-3 py-2 font-semibold">Keyframes</th>
									<th class="px-3 py-2 font-semibold">Frame gap</th>
									<th class="px-3 py-2 font-semibold">Loss / errors</th>
									<th class="px-3 py-2 font-semibold">Reconnects</th>
									<th class="px-3 py-2 font-semibold">Report</th>
								</tr>
							</thead>
							<tbody class="divide-y">
								{#each health.cameras as camera (camera.id)}
									{@const cameraVideoStreams = camera.streams.filter((stream) =>
										stream.type.startsWith('video_')
									)}
									{#if cameraVideoStreams.length === 0}
										<tr>
											<td class="px-3 py-3">
												<div class="flex items-center gap-2">
													<span class="size-1.5 rounded-full {stateColor(camera.state)}"></span>
													<span class="font-medium">{camera.name}</span>
													<span class="text-[9px] text-muted-foreground capitalize">
														{camera.state}
													</span>
													<!-- eslint-disable svelte/no-navigation-without-resolve -->
													<a
														href={cameraHref(camera)}
														class="ml-auto grid size-7 place-items-center rounded text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
														title={`Open ${camera.name} camera information`}
													>
														<CameraIcon class="size-3.5" />
														<span class="sr-only">Open {camera.name} camera information</span>
													</a>
													<!-- eslint-enable svelte/no-navigation-without-resolve -->
												</div>
												<p class="mt-0.5 font-mono text-[9px] text-muted-foreground">{camera.ip}</p>
											</td>
											<td class="px-3 py-3 font-mono text-muted-foreground" colspan="2"
												>{camera.backend ?? '—'} / {(camera.transport ?? '—').toUpperCase()}</td
											>
											<td class="px-3 py-3 text-muted-foreground" colspan="8"
												>Waiting for stream metrics</td
											>
										</tr>
									{:else}
										{#each cameraVideoStreams as stream, streamIndex (`${camera.id}-${stream.type}`)}
											<tr class="hover:bg-muted/25">
												<td class="px-3 py-2.5 align-top">
													{#if streamIndex === 0}
														<div class="flex items-center gap-2">
															<span class="size-1.5 rounded-full {stateColor(camera.state)}"></span>
															<span class="font-medium">{camera.name}</span>
															<span class="text-[9px] text-muted-foreground capitalize">
																{camera.state}
															</span>
															<!-- eslint-disable svelte/no-navigation-without-resolve -->
															<a
																href={cameraHref(camera)}
																class="ml-auto grid size-7 place-items-center rounded text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
																title={`Open ${camera.name} camera information`}
															>
																<CameraIcon class="size-3.5" />
																<span class="sr-only">Open {camera.name} camera information</span>
															</a>
															<!-- eslint-enable svelte/no-navigation-without-resolve -->
														</div>
														<p class="mt-0.5 font-mono text-[9px] text-muted-foreground">
															{camera.ip} · {camera.model ?? 'Unknown model'}
														</p>
													{/if}
												</td>
												<td class="px-3 py-2.5 align-top font-medium">{streamLabel(stream)}</td>
												<td class="px-3 py-2.5 align-top font-mono uppercase">
													{camera.backend ?? '—'} / {camera.transport ?? '—'}
												</td>
												<td class="px-3 py-2.5 align-top"
													><span class="font-mono uppercase">{stream.codec ?? '—'}</span>
													<p class="text-[9px] text-muted-foreground">
														{stream.resolution ?? '—'}
													</p></td
												>
												<td class="w-32 px-3 py-2.5 align-top">
													<span class="font-mono tabular-nums"
														>{numberFormatter.format(stream.fps ?? 0)} / {numberFormatter.format(
															stream.expected_fps ?? 0
														)}</span
													>
													<div class="mt-1 h-1 overflow-hidden bg-muted">
														<div
															class="h-full {streamUtilization(stream) < 70
																? 'bg-amber-500'
																: 'bg-emerald-500'}"
															style={`width: ${streamUtilization(stream)}%`}
														></div>
													</div>
												</td>
												<td class="px-3 py-2.5 align-top font-mono"
													>{formatBitrate((stream.kbps ?? 0) * 1_000)}
													<p class="text-[9px] text-muted-foreground">
														max {formatFrameSize(stream.max_frame_kb)}
													</p></td
												>
												<td class="px-3 py-2.5 align-top font-mono"
													>{numberFormatter.format(stream.kf_fps ?? 0)}/s
													<p class="text-[9px] text-muted-foreground">
														{compactFormatter.format(stream.keyframes ?? 0)} total
													</p>
													<p class="text-[9px] text-muted-foreground">
														{compactFormatter.format(stream.frames ?? 0)} frames · {formatBytes(
															stream.bytes
														)}
													</p></td
												>
												<td class="px-3 py-2.5 align-top font-mono"
													>min {numberFormatter.format(stream.gap_min_ms ?? 0)} · avg {numberFormatter.format(
														stream.gap_avg_ms ?? 0
													)} ms
													<p class="text-[9px] text-muted-foreground">
														max {numberFormatter.format(stream.gap_max_ms ?? 0)} ms
													</p>
													<p
														class="text-[9px] {(stream.jitter_p99_ms ?? 0) >
														(stream.expected_fps && stream.expected_fps > 0
															? 1_000 / stream.expected_fps
															: Number.POSITIVE_INFINITY)
															? 'text-amber-600 dark:text-amber-300'
															: 'text-muted-foreground'}"
													>
														jitter p50 {numberFormatter.format(stream.jitter_p50_ms ?? 0)} ms · p99
														{numberFormatter.format(stream.jitter_p99_ms ?? 0)} ms · {compactFormatter.format(
															stream.jitter_samples ?? 0
														)} samples
													</p></td
												>
												<td class="px-3 py-2.5 align-top font-mono"
													><span
														class={(stream.drops ?? 0) > 0
															? 'text-amber-600 dark:text-amber-300'
															: ''}>{compactFormatter.format(stream.drops ?? 0)}</span
													>
													/
													<span
														class={(stream.errors ?? 0) > 0 ? 'text-red-600 dark:text-red-300' : ''}
														>{compactFormatter.format(stream.errors ?? 0)}</span
													></td
												>
												<td class="px-3 py-2.5 align-top font-mono"
													>{compactFormatter.format(stream.reconnects ?? 0)}</td
												>
												<td class="px-3 py-2.5 align-top font-mono text-muted-foreground"
													>{formatAge(stream.report_age_ms)}</td
												>
											</tr>
										{/each}
									{/if}
								{/each}
							</tbody>
						</table>
					</div>
				</section>

				{#if audioStreams.length > 0}
					<section aria-labelledby="audio-streams-heading">
						<div class="mb-2 flex flex-wrap items-end justify-between gap-2">
							<div>
								<h2 id="audio-streams-heading" class="text-sm font-semibold">Audio streams</h2>
								<p class="text-[11px] text-muted-foreground">
									Measured ingress cadence and payload sizes. Keyframes do not apply to audio.
								</p>
							</div>
							<span class="font-mono text-[10px] text-muted-foreground"
								>{audioStreams.length} reporting</span
							>
						</div>
						<div class="overflow-x-auto border-y">
							<table class="w-full min-w-[62rem] text-left text-[11px]">
								<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase">
									<tr>
										<th class="px-3 py-2 font-semibold">Camera</th>
										<th class="px-3 py-2 font-semibold">Transport</th>
										<th class="px-3 py-2 font-semibold">Codec</th>
										<th class="px-3 py-2 font-semibold">FPS</th>
										<th class="px-3 py-2 font-semibold">Bitrate</th>
										<th class="px-3 py-2 font-semibold">Max frame</th>
										<th class="px-3 py-2 font-semibold">Frames / bytes</th>
										<th class="px-3 py-2 font-semibold">Keyframes</th>
										<th class="px-3 py-2 font-semibold">Report</th>
									</tr>
								</thead>
								<tbody class="divide-y">
									{#each audioStreams as entry (`${entry.camera.id}-${entry.stream.type}`)}
										<tr class="hover:bg-muted/25">
											<td class="px-3 py-2.5">
												<p class="font-medium">{entry.camera.name}</p>
												<p class="font-mono text-[9px] text-muted-foreground">{entry.camera.ip}</p>
											</td>
											<td class="px-3 py-2.5 font-mono uppercase">
												{entry.camera.backend ?? '—'} / {entry.camera.transport ?? '—'}
											</td>
											<td class="px-3 py-2.5 font-mono uppercase">{entry.stream.codec ?? '—'}</td>
											<td class="px-3 py-2.5 font-mono"
												>{numberFormatter.format(entry.stream.fps ?? 0)}</td
											>
											<td class="px-3 py-2.5 font-mono"
												>{formatBitrate((entry.stream.kbps ?? 0) * 1_000)}</td
											>
											<td class="px-3 py-2.5 font-mono"
												>{formatFrameSize(entry.stream.max_frame_kb)}</td
											>
											<td class="px-3 py-2.5 font-mono">
												{compactFormatter.format(entry.stream.frames ?? 0)} · {formatBytes(
													entry.stream.bytes
												)}
											</td>
											<td class="px-3 py-2.5 font-mono text-muted-foreground">N/A</td>
											<td class="px-3 py-2.5 font-mono text-muted-foreground"
												>{formatAge(entry.stream.report_age_ms)}</td
											>
										</tr>
									{/each}
								</tbody>
							</table>
						</div>
					</section>
				{/if}

				<section class="border-t pt-4" aria-labelledby="system-heading">
					<div class="mb-3 flex items-center gap-2">
						<ServerIcon class="size-4 text-sky-600" />
						<h2 id="system-heading" class="text-sm font-semibold">Process and host</h2>
					</div>
					<div class="grid divide-y border-y lg:grid-cols-2 lg:divide-x lg:divide-y-0">
						<div class="p-3">
							<h3 class="mb-3 text-[10px] font-semibold text-muted-foreground uppercase">
								KeepPeek process
							</h3>
							<dl class="grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-2 text-xs">
								<dt class="text-muted-foreground">Name</dt>
								<dd class="font-mono">{health.system.process.name ?? '—'}</dd>
								<dt class="text-muted-foreground">PID</dt>
								<dd class="font-mono">{health.system.process.pid}</dd>
								<dt class="text-muted-foreground">CPU host capacity</dt>
								<dd class="font-mono">
									{formatPercent(health.system.process.cpu_capacity_percent)}
								</dd>
								<dt class="text-muted-foreground">CPU core-equivalent</dt>
								<dd class="font-mono">
									{formatPercent(health.system.process.cpu_percent)} · {formatCpuCores(
										health.system.process.cpu_core_equivalents
									)}
								</dd>
								<dt class="text-muted-foreground">Host CPU</dt>
								<dd class="font-mono">{formatPercent(health.system.system_cpu_percent)}</dd>
								<dt class="text-muted-foreground">Resident memory</dt>
								<dd class="font-mono">
									{formatBytes(health.system.process.resident_memory_bytes)}
								</dd>
								<dt class="text-muted-foreground">Host RAM share</dt>
								<dd class="font-mono">
									{formatPercent(health.system.process.memory_capacity_percent)}
								</dd>
								<dt class="text-muted-foreground">Virtual address space</dt>
								<dd class="font-mono">{formatBytes(health.system.process.virtual_memory_bytes)}</dd>
								<dt class="text-muted-foreground">Tasks</dt>
								<dd class="font-mono">{health.system.process.tasks ?? '—'}</dd>
								<dt class="text-muted-foreground">Uptime</dt>
								<dd class="font-mono">{formatDuration(health.system.process.uptime_seconds)}</dd>
								<dt class="text-muted-foreground">Started</dt>
								<dd class="font-mono">
									{health.system.process.started_at_seconds === null
										? '—'
										: new Date(health.system.process.started_at_seconds * 1_000).toLocaleString()}
								</dd>
								<dt class="text-muted-foreground">Disk read</dt>
								<dd class="font-mono">
									{formatBytes(health.system.process.read_bytes_per_second)}/s
								</dd>
								<dt class="text-muted-foreground">Disk write</dt>
								<dd class="font-mono">
									{formatBytes(health.system.process.write_bytes_per_second)}/s
								</dd>
								<dt class="text-muted-foreground">Total disk read</dt>
								<dd class="font-mono">{formatBytes(health.system.process.total_read_bytes)}</dd>
								<dt class="text-muted-foreground">Total disk write</dt>
								<dd class="font-mono">{formatBytes(health.system.process.total_written_bytes)}</dd>
							</dl>
						</div>
						<div class="p-3">
							<h3 class="mb-3 text-[10px] font-semibold text-muted-foreground uppercase">
								Host resources
							</h3>
							<div class="space-y-3 text-xs">
								<div>
									<div class="mb-1 flex justify-between">
										<span>Memory</span><span class="font-mono"
											>{formatBytes(health.system.memory.used_bytes)} / {formatBytes(
												health.system.memory.total_bytes
											)}</span
										>
									</div>
									<div class="h-1.5 overflow-hidden bg-muted">
										<div
											class="h-full bg-sky-500"
											style={`width: ${Math.min(100, memoryUsedPercent)}%`}
										></div>
									</div>
									<p class="mt-1 text-[9px] text-muted-foreground">
										{formatBytes(health.system.memory.available_bytes)} available
									</p>
								</div>
								<div>
									<div class="mb-1 flex justify-between">
										<span>Process share</span><span class="font-mono"
											>{formatPercent(health.system.process.memory_capacity_percent)}</span
										>
									</div>
									<div class="h-1.5 overflow-hidden bg-muted">
										<div
											class="h-full bg-emerald-500"
											style={`width: ${Math.min(100, health.system.process.memory_capacity_percent ?? 0)}%`}
										></div>
									</div>
								</div>
								<div>
									<div class="mb-1 flex justify-between">
										<span>Swap</span><span class="font-mono"
											>{formatBytes(health.system.memory.used_swap_bytes)} / {formatBytes(
												health.system.memory.total_swap_bytes
											)}</span
										>
									</div>
									<div class="h-1.5 overflow-hidden bg-muted">
										<div
											class="h-full bg-amber-500"
											style={`width: ${health.system.memory.total_swap_bytes > 0 ? Math.min(100, (health.system.memory.used_swap_bytes / health.system.memory.total_swap_bytes) * 100) : 0}%`}
										></div>
									</div>
								</div>
								<div class="grid grid-cols-3 divide-x border-y text-center">
									<div class="p-2">
										<p class="font-mono font-semibold">
											{health.system.load.one_minute.toFixed(2)}
										</p>
										<p class="text-[9px] text-muted-foreground">1 min load</p>
									</div>
									<div class="p-2">
										<p class="font-mono font-semibold">
											{health.system.load.five_minutes.toFixed(2)}
										</p>
										<p class="text-[9px] text-muted-foreground">5 min load</p>
									</div>
									<div class="p-2">
										<p class="font-mono font-semibold">
											{health.system.load.fifteen_minutes.toFixed(2)}
										</p>
										<p class="text-[9px] text-muted-foreground">15 min load</p>
									</div>
								</div>
							</div>
						</div>
					</div>

					<div class="mt-4 overflow-x-auto border-y">
						<table class="w-full min-w-[42rem] text-left text-[11px]">
							<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase"
								><tr
									><th class="px-3 py-2">CPU</th><th class="px-3 py-2">Usage</th><th
										class="px-3 py-2">Frequency</th
									><th class="px-3 py-2">Host</th></tr
								></thead
							>
							<tbody class="divide-y"
								>{#each health.system.cpus as cpu, index (`${cpu.name}-${index}`)}<tr
										><td class="px-3 py-2 font-mono">{cpu.name}</td><td class="w-64 px-3 py-2"
											><div class="flex items-center gap-2">
												<div class="h-1.5 flex-1 overflow-hidden bg-muted">
													<div
														class="h-full bg-sky-500"
														style={`width: ${Math.min(100, cpu.usage_percent)}%`}
													></div>
												</div>
												<span class="w-12 text-right font-mono"
													>{formatPercent(cpu.usage_percent)}</span
												>
											</div></td
										><td class="px-3 py-2 font-mono">{cpu.frequency_mhz} MHz</td
										>{#if index === 0}<td
												class="px-3 py-2 text-muted-foreground"
												rowspan={health.system.cpus.length}
												>{health.system.cpu_brand ?? 'Unknown CPU'}<br />{health.system
													.physical_cores ?? '—'} physical / {health.system.logical_cores} logical cores</td
											>{/if}</tr
									>{/each}</tbody
							>
						</table>
					</div>
				</section>

				<section class="border-t pt-4" aria-labelledby="webrtc-heading">
					<div class="mb-3 flex items-center gap-2">
						<RadioIcon class="size-4 text-emerald-600" />
						<h2 id="webrtc-heading" class="text-sm font-semibold">WebRTC delivery</h2>
					</div>
					<div class="space-y-4">
						<div>
							<p class="mb-2 text-[9px] font-semibold text-muted-foreground uppercase">Current</p>
							<div
								class="grid grid-cols-2 divide-x divide-y border-y sm:grid-cols-3 xl:grid-cols-6"
							>
								{#each [['Sessions', health.webrtc.active_sessions], ['Browser', health.webrtc.browser_sessions], ['Tracks', health.webrtc.browser_tracks], ['Adaptive', health.webrtc.adaptive_sessions], ['Fixed', health.webrtc.fixed_sessions], ['Main', health.webrtc.active_main], ['Sub', health.webrtc.active_sub], ['Auto', health.webrtc.requested_auto], ['High', health.webrtc.requested_high], ['Low', health.webrtc.requested_low], ['BWE min', formatBitrate(health.webrtc.estimated_bitrate_min_bps)], ['BWE avg', formatBitrate(health.webrtc.estimated_bitrate_avg_bps)], ['BWE max', formatBitrate(health.webrtc.estimated_bitrate_max_bps)], ['Source bitrate', formatBitrate(health.webrtc.source_bitrate_bps)], ['Queued', health.webrtc.queued_frames], ['Deepest', health.webrtc.queue_depth_max], ['Capacity', health.webrtc.queue_capacity]] as metric (metric[0])}
									<div class="p-3" data-health-metric={metric[0]}>
										<p class="text-[9px] font-semibold text-muted-foreground uppercase">
											{metric[0]}
										</p>
										<p class="mt-1 font-mono text-sm font-semibold">{metric[1]}</p>
									</div>
								{/each}
							</div>
						</div>
						<div>
							<p class="mb-2 text-[9px] font-semibold text-muted-foreground uppercase">
								Since server start
							</p>
							<div
								class="grid grid-cols-2 divide-x divide-y border-y sm:grid-cols-3 xl:grid-cols-6"
							>
								{#each [['Published', compactFormatter.format(health.webrtc.published_frames)], ['Published bytes', formatBytes(health.webrtc.published_bytes)], ['Enqueued', compactFormatter.format(health.webrtc.delivered_frames)], ['Written', compactFormatter.format(health.webrtc.written_frames)], ['Peak depth', health.webrtc.queue_high_water], ['Full drops', compactFormatter.format(health.webrtc.queue_drops)], ['Discarded', compactFormatter.format(health.webrtc.queue_discarded_frames)], ['Recovery drops', compactFormatter.format(health.webrtc.queue_recovery_drops)]] as metric (metric[0])}
									<div class="p-3" data-health-metric={metric[0]}>
										<p class="text-[9px] font-semibold text-muted-foreground uppercase">
											{metric[0]}
										</p>
										<p class="mt-1 font-mono text-sm font-semibold">{metric[1]}</p>
									</div>
								{/each}
							</div>
						</div>
					</div>
					<div class="mt-4 overflow-x-auto border-y">
						<table class="w-full min-w-[74rem] text-left text-[11px]">
							<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase"
								><tr
									><th class="px-3 py-2">Session</th><th class="px-3 py-2">Track</th><th
										class="px-3 py-2">Source</th
									><th class="px-3 py-2">Stream</th><th class="px-3 py-2">Depth</th><th
										class="px-3 py-2">Peak</th
									><th class="px-3 py-2">Written</th><th class="px-3 py-2">Discarded</th><th
										class="px-3 py-2">Full drops</th
									><th class="px-3 py-2">Recovery drops</th></tr
								></thead
							>
							<tbody class="divide-y"
								>{#each health.webrtc.session_queues as queue (`${queue.session_id}-${queue.track_id ?? 'legacy'}`)}<tr
										><td class="px-3 py-2 font-mono">{queue.session_id}</td><td
											class="px-3 py-2 font-mono">{queue.track_id ?? '—'}</td
										><td class="px-3 py-2 font-mono">{queue.camera_ip}</td><td
											class="px-3 py-2 font-medium capitalize">{queue.stream}</td
										><td class="px-3 py-2 font-mono"
											>{queue.depth} / {health.webrtc.queue_capacity}</td
										><td class="px-3 py-2 font-mono">{queue.high_water}</td><td
											class="px-3 py-2 font-mono"
											>{compactFormatter.format(queue.written_frames)}</td
										><td class="px-3 py-2 font-mono">{queue.discarded_frames}</td><td
											class="px-3 py-2 font-mono">{queue.full_drops}</td
										><td class="px-3 py-2 font-mono">{queue.recovery_drops}</td></tr
									>{/each}</tbody
							>
						</table>
					</div>
					<div class="mt-4 overflow-x-auto border-y">
						<table class="w-full min-w-[52rem] text-left text-[11px]">
							<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase"
								><tr
									><th class="px-3 py-2">Source</th><th class="px-3 py-2">Stream</th><th
										class="px-3 py-2">Subscribers</th
									><th class="px-3 py-2">Source bitrate</th><th class="px-3 py-2">Keyframe</th><th
										class="px-3 py-2">Age</th
									></tr
								></thead
							>
							<tbody class="divide-y"
								>{#each health.webrtc.sources as source (`${source.camera_ip}-${source.stream}`)}<tr
										><td class="px-3 py-2 font-mono">{source.camera_ip}</td><td
											class="px-3 py-2 font-medium capitalize">{source.stream}</td
										><td class="px-3 py-2 font-mono">{source.subscribers}</td><td
											class="px-3 py-2 font-mono">{formatBitrate(source.bitrate_bps)}</td
										><td class="px-3 py-2">{source.has_keyframe ? 'Ready' : 'Waiting'}</td><td
											class="px-3 py-2 font-mono text-muted-foreground"
											>{source.keyframe_age_ms === null
												? '—'
												: formatAge(source.keyframe_age_ms)}</td
										></tr
									>{/each}</tbody
							>
						</table>
					</div>
				</section>

				<section class="border-t pt-4" aria-labelledby="storage-heading">
					<div class="mb-3 flex items-center gap-2">
						<DatabaseIcon class="size-4 text-violet-600" />
						<h2 id="storage-heading" class="text-sm font-semibold">Recording and storage</h2>
					</div>
					<div class="grid divide-y border-y lg:grid-cols-2 lg:divide-x lg:divide-y-0">
						<div class="p-3">
							<h3 class="mb-3 text-[10px] font-semibold text-muted-foreground uppercase">
								Pipeline
							</h3>
							<dl class="grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-2 text-xs">
								<dt class="text-muted-foreground">Paths</dt>
								<dd class="font-mono">{health.storage.paths_are_same ? 'Shared' : 'Separate'}</dd>
								<dt class="text-muted-foreground">Short-term window</dt>
								<dd class="font-mono">{formatDuration(health.storage.short_term_seconds)}</dd>
								<dt class="text-muted-foreground">Segment duration</dt>
								<dd class="font-mono">{formatDuration(health.storage.medium_term_seconds)}</dd>
								<dt class="text-muted-foreground">Flush interval</dt>
								<dd class="font-mono">{formatDuration(health.storage.flush_interval_seconds)}</dd>
								<dt class="text-muted-foreground">Write buffer</dt>
								<dd class="font-mono">{formatBytes(health.storage.write_buffer_bytes)}</dd>
								<dt class="text-muted-foreground">Retention limit</dt>
								<dd class="font-mono">
									{health.storage.long_term_max_bytes > 0
										? formatBytes(health.storage.long_term_max_bytes)
										: 'Unlimited'}
								</dd>
								<dt class="text-muted-foreground">Catalog size</dt>
								<dd class="font-mono">{formatBytes(health.storage.catalog_bytes)}</dd>
								<dt class="text-muted-foreground">Active streams</dt>
								<dd class="font-mono">{health.storage.demand.active_streams}</dd>
								<dt class="text-muted-foreground">Viewers</dt>
								<dd class="font-mono">{health.storage.demand.total_viewers}</dd>
								<dt class="text-muted-foreground">Leased streams</dt>
								<dd class="font-mono">{health.storage.demand.leased_streams}</dd>
							</dl>
						</div>
						<div class="p-3">
							<h3 class="mb-3 text-[10px] font-semibold text-muted-foreground uppercase">
								Turso catalog
							</h3>
							{#if health.storage.catalog}<dl
									class="grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-2 text-xs"
								>
									<dt class="text-muted-foreground">Recording files</dt>
									<dd class="font-mono">
										{compactFormatter.format(health.storage.catalog.recording_files)}
									</dd>
									<dt class="text-muted-foreground">Finalized / active</dt>
									<dd class="font-mono">
										{compactFormatter.format(health.storage.catalog.finalized_files)} / {compactFormatter.format(
											health.storage.catalog.active_files
										)}
									</dd>
									<dt class="text-muted-foreground">Fragments</dt>
									<dd class="font-mono">
										{compactFormatter.format(health.storage.catalog.fragments)}
									</dd>
									<dt class="text-muted-foreground">Fragment bytes</dt>
									<dd class="font-mono">{formatBytes(health.storage.catalog.fragment_bytes)}</dd>
									<dt class="text-muted-foreground">Events / open</dt>
									<dd class="font-mono">
										{compactFormatter.format(health.storage.catalog.events)} / {compactFormatter.format(
											health.storage.catalog.open_events
										)}
									</dd>
									<dt class="text-muted-foreground">Thumbnails</dt>
									<dd class="font-mono">
										{compactFormatter.format(health.storage.catalog.event_thumbnails)}
									</dd>
								</dl>{:else}<p class="text-xs text-muted-foreground">
									Catalog metrics unavailable
								</p>{/if}
						</div>
					</div>
					<div class="mt-4 overflow-x-auto border-y">
						<table class="w-full min-w-[58rem] text-left text-[11px]">
							<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase"
								><tr
									><th class="px-3 py-2">Mount</th><th class="px-3 py-2">Device</th><th
										class="px-3 py-2">Filesystem</th
									><th class="px-3 py-2">Used</th><th class="px-3 py-2">Available</th><th
										class="px-3 py-2">Utilization</th
									><th class="px-3 py-2">Role</th></tr
								></thead
							><tbody class="divide-y"
								>{#each health.system.disks as disk (`${disk.mount_point}-${disk.name}`)}<tr
										><td class="px-3 py-2 font-mono">{disk.mount_point}</td><td class="px-3 py-2"
											>{disk.name || '—'}
											<p class="text-[9px] text-muted-foreground capitalize">{disk.kind}</p></td
										><td class="px-3 py-2 font-mono uppercase">{disk.file_system || '—'}</td><td
											class="px-3 py-2 font-mono"
											>{formatBytes(disk.used_bytes)} / {formatBytes(disk.total_bytes)}</td
										><td class="px-3 py-2 font-mono">{formatBytes(disk.available_bytes)}</td><td
											class="w-44 px-3 py-2"
											><div class="h-1.5 overflow-hidden bg-muted">
												<div
													class="h-full {diskFreePercent(disk) < 10
														? 'bg-red-500'
														: diskFreePercent(disk) < 20
															? 'bg-amber-500'
															: 'bg-emerald-500'}"
													style={`width: ${100 - diskFreePercent(disk)}%`}
												></div>
											</div></td
										><td class="px-3 py-2"
											>{disk.stores_recordings
												? 'Recordings'
												: disk.removable
													? 'Removable'
													: 'System'}</td
										></tr
									>{/each}</tbody
							>
						</table>
					</div>
					{#if health.storage.demand.streams.length > 0}<div class="mt-4 overflow-x-auto border-y">
							<table class="w-full min-w-[36rem] text-left text-[11px]">
								<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase"
									><tr
										><th class="px-3 py-2">Demand stream</th><th class="px-3 py-2">Viewers</th><th
											class="px-3 py-2">Lease remaining</th
										></tr
									></thead
								><tbody class="divide-y"
									>{#each health.storage.demand.streams as stream (stream.stream_id)}<tr
											><td class="px-3 py-2 font-mono">{stream.stream_id}</td><td
												class="px-3 py-2 font-mono">{stream.viewers}</td
											><td class="px-3 py-2 font-mono"
												>{stream.lease_remaining_ms === null
													? '—'
													: formatAge(stream.lease_remaining_ms)}</td
											></tr
										>{/each}</tbody
								>
							</table>
						</div>{/if}
					<div class="mt-3 space-y-1 font-mono text-[10px] text-muted-foreground">
						<p class="break-all">Medium: {health.storage.medium_term_path}</p>
						<p class="break-all">Long: {health.storage.long_term_path}</p>
					</div>
				</section>

				{#if health.system.temperatures.length > 0}
					<section class="border-t pt-4" aria-labelledby="temperature-heading">
						<div class="mb-3 flex items-center gap-2">
							<ThermometerIcon class="size-4 text-rose-600" />
							<h2 id="temperature-heading" class="text-sm font-semibold">Temperatures</h2>
						</div>
						<div class="overflow-x-auto border-y">
							<table class="w-full min-w-[36rem] text-left text-[11px]">
								<thead class="bg-muted/40 text-[9px] text-muted-foreground uppercase"
									><tr
										><th class="px-3 py-2">Sensor</th><th class="px-3 py-2">Current</th><th
											class="px-3 py-2">Maximum</th
										><th class="px-3 py-2">Critical</th></tr
									></thead
								><tbody class="divide-y"
									>{#each health.system.temperatures as temperature (temperature.label)}<tr
											><td class="px-3 py-2">{temperature.label}</td><td class="px-3 py-2 font-mono"
												>{formatTemperature(temperature.current_celsius)}</td
											><td class="px-3 py-2 font-mono"
												>{formatTemperature(temperature.max_celsius)}</td
											><td class="px-3 py-2 font-mono"
												>{formatTemperature(temperature.critical_celsius)}</td
											></tr
										>{/each}</tbody
								>
							</table>
						</div>
					</section>
				{/if}

				<section class="border-t pt-4" aria-labelledby="runtime-heading">
					<div class="mb-3 flex items-center gap-2">
						<CpuIcon class="size-4 text-muted-foreground" />
						<h2 id="runtime-heading" class="text-sm font-semibold">Runtime identity</h2>
					</div>
					<div
						class="grid divide-y border-y text-xs md:grid-cols-2 md:divide-x md:divide-y-0 xl:grid-cols-4"
					>
						<div class="p-3">
							<p class="text-[9px] text-muted-foreground uppercase">Host</p>
							<p class="mt-1 font-medium">{health.system.host_name ?? 'Unknown'}</p>
							<p class="font-mono text-[10px] text-muted-foreground">
								{health.system.architecture}
							</p>
						</div>
						<div class="p-3">
							<p class="text-[9px] text-muted-foreground uppercase">Operating system</p>
							<p class="mt-1 font-medium">
								{health.system.os_version ?? health.system.os_name ?? 'Unknown'}
							</p>
							<p class="font-mono text-[10px] text-muted-foreground">
								Kernel {health.system.kernel_version ?? '—'}
							</p>
						</div>
						<div class="p-3">
							<p class="text-[9px] text-muted-foreground uppercase">Executable</p>
							<p
								class="mt-1 truncate font-mono text-[10px]"
								title={health.system.process.executable ?? ''}
							>
								{health.system.process.executable ?? '—'}
							</p>
							<p
								class="truncate font-mono text-[10px] text-muted-foreground"
								title={health.system.process.working_directory ?? ''}
							>
								{health.system.process.working_directory ?? '—'}
							</p>
						</div>
						<div class="p-3">
							<p class="text-[9px] text-muted-foreground uppercase">System uptime</p>
							<p class="mt-1 font-medium">{formatDuration(health.system.system_uptime_seconds)}</p>
							<p class="font-mono text-[10px] text-muted-foreground">
								Boot {new Date(health.system.boot_time_seconds * 1_000).toLocaleString()}
							</p>
						</div>
					</div>
				</section>
			</div>
		{/if}
	{/if}
</div>
