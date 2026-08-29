<script lang="ts">
	import { onMount } from 'svelte';
	import { observeGridVisibility, type GridTileVisibility } from '$lib/grid-visibility';
	import { useLivePeer } from '$lib/stream-peer-context';
	import type { LiveQuality } from '$lib/types';
	import { emitTimelinePerformanceEvent } from '$lib/timeline-observability';
	import InfoIcon from '@lucide/svelte/icons/info';
	import { Popover } from 'bits-ui';

	type ExtendedInboundStats = RTCInboundRtpStreamStats & {
		powerEfficientDecoder?: boolean;
	};

	type StatsSample = {
		ssrc: number;
		timestamp: number;
		bytesReceived: number;
		framesReceived: number;
		videoFrames: number;
		presentedFrames: number;
	};

	type VideoDiagnostics = {
		width: number | null;
		height: number | null;
		streamFramesPerSecond: number | null;
		framesPerSecond: number | null;
		receiveBitrateBps: number | null;
		packetsReceived: number;
		packetsLost: number;
		packetLossPercent: number | null;
		framesDecoded: number;
		presentedFramesPerSecond: number | null;
		keyFramesDecoded: number;
		rtcFramesDropped: number;
		renderFramesDropped: number;
		freezeCount: number;
		totalFreezeDurationSeconds: number;
		jitterMs: number | null;
		jitterBufferMs: number | null;
		roundTripTimeMs: number | null;
		averageDecodeTimeMs: number | null;
		nackCount: number;
		pliCount: number;
		decoderImplementation: string | null;
		powerEfficientDecoder: boolean | null;
	};

	const EMPTY_DIAGNOSTICS: VideoDiagnostics = {
		width: null,
		height: null,
		streamFramesPerSecond: null,
		framesPerSecond: null,
		receiveBitrateBps: null,
		packetsReceived: 0,
		packetsLost: 0,
		packetLossPercent: null,
		framesDecoded: 0,
		presentedFramesPerSecond: null,
		keyFramesDecoded: 0,
		rtcFramesDropped: 0,
		renderFramesDropped: 0,
		freezeCount: 0,
		totalFreezeDurationSeconds: 0,
		jitterMs: null,
		jitterBufferMs: null,
		roundTripTimeMs: null,
		averageDecodeTimeMs: null,
		nackCount: 0,
		pliCount: 0,
		decoderImplementation: null,
		powerEfficientDecoder: null
	};

	const compactNumber = new Intl.NumberFormat(undefined, {
		notation: 'compact',
		maximumFractionDigits: 1
	});

	type Props = {
		cameraId: string;
		stream: 'main' | 'sub';
		quality?: LiveQuality;
		matchVideoAspectRatio?: boolean;
		showDiagnostics?: boolean;
		diagnosticsLabel?: string;
		diagnosticsStatusClass?: string;
		diagnosticsPosition?: 'top-right' | 'bottom-right';
		diagnosticsRecording?: {
			state: 'recording' | 'not-progressing' | 'pending' | 'off' | 'unknown';
			detail: string;
			sessionDurationMs: number | null;
			mainDurationMs: number | null;
			subDurationMs: number | null;
			totalDurationMs: number | null;
		};
		onframeactivitychange?: (active: boolean) => void;
		onvisibilitychange?: (visibility: GridTileVisibility) => void;
		class?: string;
	};

	let {
		cameraId,
		stream,
		quality,
		matchVideoAspectRatio = false,
		showDiagnostics = true,
		diagnosticsLabel,
		diagnosticsStatusClass = 'bg-white/65',
		diagnosticsPosition = 'top-right',
		diagnosticsRecording,
		onframeactivitychange,
		onvisibilitychange,
		class: className = ''
	}: Props = $props();

	const livePeer = useLivePeer();
	let track = $derived(livePeer.track(cameraId));
	let container: HTMLDivElement | null = $state(null);
	let video = $state<HTMLVideoElement | null>(null);
	let videoAspectRatio = $state<number | null>(null);
	let negotiatedCodec = $state<string | null>(null);
	let diagnosticsOpen = $state(false);
	let diagnostics = $state.raw<VideoDiagnostics>(EMPTY_DIAGNOSTICS);
	let previousStatsSample: StatsSample | null = null;
	let dropBaseline: {
		source: string;
		rtcFramesDropped: number;
		renderFramesDropped: number;
	} | null = null;
	let statsRefreshInFlight = false;
	let presentedFrames = 0;
	let lastPresentedFrameAt = 0;
	let frameActivityActive = $state(false);
	let frozenFrameUrl = $state<string | null>(null);
	let mounted = false;
	// The compositor discards every frame while the tab is hidden; those are not render drops.
	let renderDropsNeedRebaseline = false;
	let status = $derived(track?.status ?? 'connecting');
	let sessionId = $derived(livePeer.sessionId);
	let activeStream = $derived(track?.activeStream ?? 'sub');
	let estimatedBitrateBps = $derived(track?.estimatedBitrateBps ?? livePeer.estimatedBitrateBps);
	let connectionState = $derived(livePeer.connectionState);
	let iceConnectionState = $derived(livePeer.iceConnectionState);
	let receiver = $derived(track?.receiver ?? null);
	let requestedQuality = $derived<LiveQuality>(
		track?.requestedQuality ?? quality ?? (stream === 'main' ? 'auto' : 'low')
	);
	let resolution = $derived(
		diagnostics.width && diagnostics.height
			? `${diagnostics.width} × ${diagnostics.height}`
			: video?.videoWidth && video.videoHeight
				? `${video.videoWidth} × ${video.videoHeight}`
				: '—'
	);
	let packetLoss = $derived(
		diagnostics.packetLossPercent === null
			? compactNumber.format(diagnostics.packetsLost)
			: `${compactNumber.format(diagnostics.packetsLost)} (${diagnostics.packetLossPercent.toFixed(2)}%)`
	);
	let diagnosticsAccessibleLabel = $derived(
		diagnosticsLabel ? `${diagnosticsLabel} camera information` : 'WebRTC stream diagnostics'
	);

	onMount(() => {
		mounted = true;
		const detach = livePeer.attach(cameraId);
		return () => {
			mounted = false;
			detach();
			if (frozenFrameUrl) URL.revokeObjectURL(frozenFrameUrl);
		};
	});

	onMount(() => {
		if (!container || !onvisibilitychange) return;
		return observeGridVisibility(container, cameraId, onvisibilitychange);
	});

	$effect(() => {
		if (!video) return;
		const stream = track?.stream ?? null;
		if (video.srcObject !== stream) {
			if (video.srcObject && !stream) void captureFrozenFrame(video);
			video.srcObject = stream;
			video.load();
		}
		if (stream) {
			video.autoplay = true;
			video.muted = true;
			video.playsInline = true;
			void video.play().catch(() => {});
		}
	});

	$effect(() => {
		const element = video;
		if (!element) return;
		let active = true;
		let handle: number | null = null;
		if (typeof element.requestVideoFrameCallback === 'function') {
			handle = element.requestVideoFrameCallback(function onFrame(_now, metadata) {
				presentedFrames = metadata.presentedFrames;
				markFrameActivity();
				if (active) handle = element.requestVideoFrameCallback(onFrame);
			});
		}
		const monitor = window.setInterval(() => {
			if (frameActivityActive && performance.now() - lastPresentedFrameAt >= 5_000) {
				setFrameActivity(false);
			}
		}, 500);
		return () => {
			active = false;
			if (handle !== null) element.cancelVideoFrameCallback?.(handle);
			window.clearInterval(monitor);
			setFrameActivity(false);
		};
	});

	$effect(() => {
		const onVisibility = () => {
			if (document.visibilityState === 'visible') renderDropsNeedRebaseline = true;
		};
		document.addEventListener('visibilitychange', onVisibility);
		return () => document.removeEventListener('visibilitychange', onVisibility);
	});

	$effect(() => {
		if (!diagnosticsOpen) {
			previousStatsSample = null;
			return;
		}
		void refreshReceiverStats(true);
		const statsTimer = window.setInterval(() => void refreshReceiverStats(true), 1_000);
		return () => {
			window.clearInterval(statsTimer);
		};
	});

	async function handlePlaying() {
		markFrameActivity();
		refreshVideoAspectRatio();
		livePeer.markPlaying(cameraId);
		if (frozenFrameUrl) {
			URL.revokeObjectURL(frozenFrameUrl);
			frozenFrameUrl = null;
		}
		void refreshReceiverStats(false);
	}

	function refreshVideoAspectRatio() {
		const width = video?.videoWidth ?? 0;
		const height = video?.videoHeight ?? 0;
		videoAspectRatio = width > 0 && height > 0 ? width / height : null;
	}

	function handleVideoResize() {
		refreshVideoAspectRatio();
		void refreshReceiverStats(false);
	}

	async function captureFrozenFrame(element: HTMLVideoElement): Promise<void> {
		if (element.videoWidth <= 0 || element.videoHeight <= 0) return;
		const scale = Math.min(1, 640 / Math.max(element.videoWidth, element.videoHeight));
		const canvas = document.createElement('canvas');
		canvas.width = Math.max(1, Math.round(element.videoWidth * scale));
		canvas.height = Math.max(1, Math.round(element.videoHeight * scale));
		const context = canvas.getContext('2d');
		if (!context) return;
		context.drawImage(element, 0, 0, canvas.width, canvas.height);
		const blob = await new Promise<Blob | null>((resolve) =>
			canvas.toBlob(resolve, 'image/jpeg', 0.78)
		);
		if (!blob || !mounted || element.srcObject) return;
		const url = URL.createObjectURL(blob);
		if (frozenFrameUrl) URL.revokeObjectURL(frozenFrameUrl);
		frozenFrameUrl = url;
		emitTimelinePerformanceEvent('GridTileFrozen', { sourceId: cameraId });
	}

	function markFrameActivity(): void {
		lastPresentedFrameAt = performance.now();
		setFrameActivity(true);
	}

	function setFrameActivity(active: boolean): void {
		if (frameActivityActive === active) return;
		frameActivityActive = active;
		onframeactivitychange?.(active);
	}

	function handlePlaybackInactive(): void {
		setFrameActivity(false);
	}

	async function refreshReceiverStats(measureRate: boolean) {
		if (!receiver || statsRefreshInFlight) return;
		statsRefreshInFlight = true;
		try {
			const stats = await receiver.getStats();
			if (!mounted) return;
			const inbound = [...stats.values()].find(
				(report) => report.type === 'inbound-rtp' && report.kind === 'video'
			) as ExtendedInboundStats | undefined;
			if (!inbound) return;

			if (inbound.codecId) {
				const codec = stats.get(inbound.codecId) as { type: string; mimeType?: string } | undefined;
				if (codec?.type === 'codec' && codec.mimeType) {
					negotiatedCodec = codec.mimeType.toLowerCase();
				}
			}

			const sample: StatsSample = {
				ssrc: inbound.ssrc,
				timestamp: inbound.timestamp,
				bytesReceived: (inbound.bytesReceived ?? 0) + (inbound.headerBytesReceived ?? 0),
				framesReceived: inbound.framesReceived ?? 0,
				videoFrames: Math.max(
					video?.getVideoPlaybackQuality().totalVideoFrames ?? 0,
					inbound.framesDecoded ?? 0,
					presentedFrames
				),
				presentedFrames
			};
			let receiveBitrateBps = diagnostics.receiveBitrateBps;
			let streamFramesPerSecond = diagnostics.streamFramesPerSecond;
			let measuredFramesPerSecond = diagnostics.framesPerSecond;
			let presentedFramesPerSecond = diagnostics.presentedFramesPerSecond;
			if (
				measureRate &&
				previousStatsSample?.ssrc === sample.ssrc &&
				sample.timestamp > previousStatsSample.timestamp
			) {
				const elapsedSeconds = (sample.timestamp - previousStatsSample.timestamp) / 1_000;
				receiveBitrateBps =
					(Math.max(0, sample.bytesReceived - previousStatsSample.bytesReceived) * 8) /
					elapsedSeconds;
				streamFramesPerSecond =
					Math.max(0, sample.framesReceived - previousStatsSample.framesReceived) / elapsedSeconds;
				measuredFramesPerSecond =
					Math.max(0, sample.videoFrames - previousStatsSample.videoFrames) / elapsedSeconds;
				presentedFramesPerSecond =
					Math.max(0, sample.presentedFrames - previousStatsSample.presentedFrames) /
					elapsedSeconds;
			}
			if (measureRate) previousStatsSample = sample;

			const packetsReceived = inbound.packetsReceived ?? 0;
			const packetsLost = Math.max(0, inbound.packetsLost ?? 0);
			const packetTotal = packetsReceived + packetsLost;
			const transport = inbound.transportId
				? (stats.get(inbound.transportId) as RTCTransportStats | undefined)
				: undefined;
			const candidatePair = transport?.selectedCandidatePairId
				? (stats.get(transport.selectedCandidatePairId) as RTCIceCandidatePairStats | undefined)
				: undefined;
			const jitterBufferEmittedCount = inbound.jitterBufferEmittedCount ?? 0;
			const playbackQuality = video?.getVideoPlaybackQuality();
			const rtcFramesDropped = inbound.framesDropped ?? 0;
			const renderFramesDropped = playbackQuality?.droppedVideoFrames ?? 0;
			const dropSource = [
				inbound.ssrc,
				inbound.codecId,
				activeStream,
				inbound.frameWidth,
				inbound.frameHeight
			].join(':');
			if (dropBaseline === null) {
				dropBaseline = {
					source: dropSource,
					rtcFramesDropped: 0,
					renderFramesDropped: 0
				};
			} else if (dropBaseline.source !== dropSource) {
				dropBaseline = { source: dropSource, rtcFramesDropped, renderFramesDropped };
			}
			// Frames discarded while the tab was hidden were never composited, so they
			// are not evidence of a rendering problem.
			if (renderDropsNeedRebaseline) {
				dropBaseline.renderFramesDropped = renderFramesDropped;
				renderDropsNeedRebaseline = false;
			}
			const framesDecoded = Math.max(
				inbound.framesDecoded ?? 0,
				playbackQuality?.totalVideoFrames ?? 0
			);

			diagnostics = {
				width: inbound.frameWidth ?? video?.videoWidth ?? null,
				height: inbound.frameHeight ?? video?.videoHeight ?? null,
				streamFramesPerSecond,
				framesPerSecond:
					inbound.framesPerSecond && inbound.framesPerSecond > 0
						? inbound.framesPerSecond
						: measuredFramesPerSecond,
				receiveBitrateBps,
				packetsReceived,
				packetsLost,
				packetLossPercent: packetTotal > 0 ? (packetsLost / packetTotal) * 100 : null,
				framesDecoded,
				presentedFramesPerSecond,
				keyFramesDecoded: inbound.keyFramesDecoded ?? 0,
				rtcFramesDropped: Math.max(0, rtcFramesDropped - dropBaseline.rtcFramesDropped),
				renderFramesDropped: Math.max(0, renderFramesDropped - dropBaseline.renderFramesDropped),
				freezeCount: inbound.freezeCount ?? 0,
				totalFreezeDurationSeconds: inbound.totalFreezesDuration ?? 0,
				jitterMs: inbound.jitter === undefined ? null : inbound.jitter * 1_000,
				jitterBufferMs:
					jitterBufferEmittedCount > 0 && inbound.jitterBufferDelay !== undefined
						? (inbound.jitterBufferDelay / jitterBufferEmittedCount) * 1_000
						: null,
				roundTripTimeMs:
					candidatePair?.currentRoundTripTime === undefined
						? null
						: candidatePair.currentRoundTripTime * 1_000,
				averageDecodeTimeMs:
					framesDecoded > 0 && inbound.totalDecodeTime !== undefined
						? (inbound.totalDecodeTime / framesDecoded) * 1_000
						: null,
				nackCount: inbound.nackCount ?? 0,
				pliCount: inbound.pliCount ?? 0,
				decoderImplementation: inbound.decoderImplementation ?? null,
				powerEfficientDecoder: inbound.powerEfficientDecoder ?? null
			};
		} finally {
			statsRefreshInFlight = false;
		}
	}

	function formatBitrate(bitsPerSecond: number | null): string {
		if (bitsPerSecond === null) return '—';
		if (bitsPerSecond >= 1_000_000) return `${(bitsPerSecond / 1_000_000).toFixed(1)} Mbps`;
		if (bitsPerSecond >= 1_000) return `${Math.round(bitsPerSecond / 1_000)} kbps`;
		return `${Math.round(bitsPerSecond)} bps`;
	}

	function formatMilliseconds(milliseconds: number | null): string {
		return milliseconds === null ? '—' : `${milliseconds.toFixed(milliseconds < 10 ? 1 : 0)} ms`;
	}

	function formatFramesPerSecond(framesPerSecond: number | null): string {
		return framesPerSecond === null ? '—' : framesPerSecond.toFixed(1);
	}

	function formatDuration(milliseconds: number | null): string {
		if (milliseconds === null) return '—';
		const totalSeconds = Math.floor(milliseconds / 1_000);
		const days = Math.floor(totalSeconds / 86_400);
		const hours = Math.floor((totalSeconds % 86_400) / 3_600);
		const minutes = Math.floor((totalSeconds % 3_600) / 60);
		const seconds = totalSeconds % 60;
		if (days > 0)
			return `${days}d ${hours.toString().padStart(2, '0')}h ${minutes.toString().padStart(2, '0')}m`;
		if (hours > 0)
			return `${hours}h ${minutes.toString().padStart(2, '0')}m ${seconds.toString().padStart(2, '0')}s`;
		if (minutes > 0) return `${minutes}m ${seconds.toString().padStart(2, '0')}s`;
		return `${seconds}s`;
	}

	function recordingIndicatorClass(
		state: NonNullable<Props['diagnosticsRecording']>['state']
	): string {
		if (state === 'recording') return 'bg-emerald-400';
		if (state === 'not-progressing') return 'bg-red-400';
		if (state === 'pending') return 'bg-amber-400';
		return 'bg-white/35';
	}

	function handleDiagnosticsEscape(event: KeyboardEvent) {
		event.stopPropagation();
	}
</script>

<div
	bind:this={container}
	class="relative bg-video {className}"
	style={matchVideoAspectRatio && videoAspectRatio !== null
		? `aspect-ratio: ${videoAspectRatio}`
		: undefined}
	data-status={status}
	data-camera-id={cameraId}
	data-session-id={sessionId}
	data-stream={activeStream}
	data-pending-stream={track?.pendingStream ?? undefined}
	data-requested-quality={requestedQuality}
	data-requested-variant={track?.requestedVariantId ?? undefined}
	data-estimated-bitrate-bps={estimatedBitrateBps}
	data-decoder="browser"
	data-codec={negotiatedCodec}
	data-frame-activity={frameActivityActive ? 'active' : 'idle'}
>
	<video
		bind:this={video}
		autoplay
		playsinline
		muted
		onplaying={handlePlaying}
		onloadeddata={markFrameActivity}
		onloadedmetadata={handleVideoResize}
		onwaiting={handlePlaybackInactive}
		onstalled={handlePlaybackInactive}
		onpause={handlePlaybackInactive}
		onemptied={handlePlaybackInactive}
		ontimeupdate={() => {
			if (typeof video?.requestVideoFrameCallback !== 'function') markFrameActivity();
		}}
		onresize={handleVideoResize}
		onerror={() => {
			handlePlaybackInactive();
			livePeer.markUnavailable(cameraId);
		}}
		class="h-full w-full object-contain"
	></video>
	{#if frozenFrameUrl && status !== 'live'}
		<img
			src={frozenFrameUrl}
			alt=""
			class="pointer-events-none absolute inset-0 z-10 size-full object-contain"
		/>
	{/if}
	<Popover.Root bind:open={diagnosticsOpen}>
		<Popover.Trigger
			data-peek-camera-label={diagnosticsLabel ?? undefined}
			class="absolute right-2 z-30 rounded-sm border border-white/15 bg-black/65 text-white/65 shadow-sm backdrop-blur-sm hover:bg-black/85 hover:text-white focus-visible:ring-2 focus-visible:ring-white/70 focus-visible:outline-none {diagnosticsPosition ===
			'bottom-right'
				? 'bottom-2'
				: 'top-2'} {diagnosticsLabel
				? 'inline-flex h-[26px] max-w-[calc(100%-1rem)] items-center gap-2 px-2'
				: 'grid size-6 place-items-center'} {showDiagnostics ? '' : 'hidden'} {diagnosticsOpen
				? 'bg-black/90 text-white ring-1 ring-white/35'
				: ''}"
			aria-label={diagnosticsAccessibleLabel}
		>
			{#if diagnosticsLabel}
				<span class="size-1.5 shrink-0 rounded-full {diagnosticsStatusClass}"></span>
				<span class="max-w-36 truncate text-xs font-medium">{diagnosticsLabel}</span>
				<span class="h-3.5 w-px shrink-0 bg-white/15"></span>
			{/if}
			<InfoIcon class="size-3.5 shrink-0" />
		</Popover.Trigger>
		<Popover.Portal>
			<Popover.Content
				role="dialog"
				side="left"
				align="start"
				sideOffset={6}
				collisionPadding={8}
				class="z-50 max-h-[calc(100vh-1rem)] w-72 max-w-[calc(100vw-1rem)] overflow-y-auto rounded-md border border-white/15 bg-zinc-950/97 p-3 text-left text-[11px] text-white shadow-2xl backdrop-blur-md"
				aria-label={diagnosticsAccessibleLabel}
				trapFocus={false}
				onEscapeKeydown={handleDiagnosticsEscape}
			>
				<div data-web-rtc-diagnostics={cameraId}>
					<div class="mb-2 flex items-center justify-between gap-3 border-b border-white/10 pb-2">
						<div class="flex items-center gap-2">
							<span
								class="size-1.5 rounded-full {status === 'live'
									? 'bg-emerald-400'
									: 'bg-amber-400'}"
							></span>
							<span class="font-semibold text-white"
								>{diagnosticsLabel ? 'Camera info' : 'WebRTC'}</span
							>
						</div>
						<span class="font-mono text-[10px] text-white/45">#{sessionId ?? '—'}</span>
					</div>

					<dl class="grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-1.5">
						{#if diagnosticsRecording}
							<dt class="text-white/45">Recording</dt>
							<dd
								data-web-rtc-recording={cameraId}
								data-recording-state={diagnosticsRecording.state}
								class="flex max-w-44 items-center justify-end gap-1.5 text-right font-medium"
							>
								<span
									class="size-1.5 shrink-0 rounded-full {recordingIndicatorClass(
										diagnosticsRecording.state
									)}"
								></span>
								<span>{diagnosticsRecording.detail}</span>
							</dd>
							<dt class="text-white/45">Camera session</dt>
							<dd data-camera-session-duration={cameraId} class="font-mono">
								{formatDuration(diagnosticsRecording.sessionDurationMs)}
							</dd>
							<dt class="text-white/45">Main recorded</dt>
							<dd data-main-recorded-duration={cameraId} class="font-mono">
								{formatDuration(diagnosticsRecording.mainDurationMs)}
							</dd>
							<dt class="text-white/45">Sub recorded</dt>
							<dd data-sub-recorded-duration={cameraId} class="font-mono">
								{formatDuration(diagnosticsRecording.subDurationMs)}
							</dd>
							<dt class="text-white/45">Total recorded</dt>
							<dd data-total-recorded-duration={cameraId} class="font-mono">
								{formatDuration(diagnosticsRecording.totalDurationMs)}
							</dd>
							<div class="col-span-2 my-1 border-t border-white/10"></div>
						{/if}
						<dt class="text-white/45">Quality</dt>
						<dd class="text-right font-medium capitalize">{requestedQuality} · {activeStream}</dd>
						<dt class="text-white/45">Codec</dt>
						<dd class="font-mono uppercase">{negotiatedCodec?.replace('video/', '') ?? '—'}</dd>
						<dt class="text-white/45">Resolution</dt>
						<dd class="font-mono">{resolution}</dd>
						<dt class="text-white/45">Stream FPS</dt>
						<dd class="font-mono">
							{formatFramesPerSecond(diagnostics.streamFramesPerSecond)}
						</dd>
						<dt class="text-white/45">Decoded FPS</dt>
						<dd class="font-mono">{formatFramesPerSecond(diagnostics.framesPerSecond)}</dd>
						<dt class="text-white/45">Receive bitrate</dt>
						<dd class="font-mono" data-web-rtc-metric="receive-bitrate">
							{formatBitrate(diagnostics.receiveBitrateBps)}
						</dd>
						<dt class="text-white/45">Server capacity</dt>
						<dd class="font-mono">{formatBitrate(estimatedBitrateBps)}</dd>

						<div class="col-span-2 my-1 border-t border-white/10"></div>

						<dt class="text-white/45">Packets received</dt>
						<dd class="font-mono">{compactNumber.format(diagnostics.packetsReceived)}</dd>
						<dt class="text-white/45">Packets lost</dt>
						<dd class="font-mono {diagnostics.packetsLost > 0 ? 'text-amber-300' : ''}">
							{packetLoss}
						</dd>
						<dt class="text-white/45">Jitter</dt>
						<dd class="font-mono">{formatMilliseconds(diagnostics.jitterMs)}</dd>
						<dt class="text-white/45">Jitter buffer</dt>
						<dd class="font-mono">{formatMilliseconds(diagnostics.jitterBufferMs)}</dd>
						<dt class="text-white/45">Round trip</dt>
						<dd class="font-mono">{formatMilliseconds(diagnostics.roundTripTimeMs)}</dd>
						<dt class="text-white/45">Feedback</dt>
						<dd class="font-mono">{diagnostics.pliCount} PLI · {diagnostics.nackCount} NACK</dd>

						<div class="col-span-2 my-1 border-t border-white/10"></div>

						<dt class="text-white/45">Frames decoded</dt>
						<dd class="font-mono">{compactNumber.format(diagnostics.framesDecoded)}</dd>
						<dt class="text-white/45">Presented</dt>
						<dd class="font-mono">
							{diagnostics.presentedFramesPerSecond === null
								? '—'
								: `${diagnostics.presentedFramesPerSecond.toFixed(1)} fps`}
						</dd>
						<dt class="text-white/45">Keyframes decoded</dt>
						<dd class="font-mono">{compactNumber.format(diagnostics.keyFramesDecoded)}</dd>
						<dt class="text-white/45">RTC drops</dt>
						<dd class="font-mono {diagnostics.rtcFramesDropped > 0 ? 'text-amber-300' : ''}">
							{compactNumber.format(diagnostics.rtcFramesDropped)}
						</dd>
						<dt class="text-white/45">Render drops</dt>
						<dd class="font-mono {diagnostics.renderFramesDropped > 0 ? 'text-amber-300' : ''}">
							{compactNumber.format(diagnostics.renderFramesDropped)}
						</dd>
						<dt class="text-white/45">Decode time</dt>
						<dd class="font-mono">{formatMilliseconds(diagnostics.averageDecodeTimeMs)}</dd>
						<dt class="text-white/45">Freezes</dt>
						<dd class="font-mono">
							{diagnostics.freezeCount} · {diagnostics.totalFreezeDurationSeconds.toFixed(1)} s
						</dd>
						<dt class="text-white/45">Decoder</dt>
						<dd
							class="max-w-40 truncate text-right"
							title={diagnostics.decoderImplementation ?? ''}
						>
							{diagnostics.decoderImplementation ?? 'Browser'}{diagnostics.powerEfficientDecoder ===
							true
								? ' · HW'
								: ''}
						</dd>
						<dt class="text-white/45">Connection</dt>
						<dd class="font-mono">{connectionState} · {iceConnectionState}</dd>
					</dl>
				</div>
			</Popover.Content>
		</Popover.Portal>
	</Popover.Root>
	{#if status === 'unavailable'}
		<div class="absolute inset-0 grid place-items-center bg-black/70">
			<span class="text-xs font-medium text-white/70">Live view unavailable</span>
		</div>
	{:else if status === 'queued'}
		<div class="absolute inset-0 z-20 grid place-items-center bg-black/35">
			<span class="text-xs font-medium text-white/70">Queued</span>
		</div>
	{/if}
</div>
