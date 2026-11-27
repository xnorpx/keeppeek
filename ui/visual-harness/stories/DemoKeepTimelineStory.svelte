<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import KeepExportPanel from '$lib/components/KeepExportPanel.svelte';
	import KeepStories from '$lib/components/KeepStories.svelte';
	import KeepSwimlanes from '$lib/components/KeepSwimlanes.svelte';
	import TimelapseReview from '$lib/components/TimelapseReview.svelte';
	import VerticalTimeline from '$lib/components/VerticalTimeline.svelte';
	import { ControlClient, type MediaExportJob } from '$lib/control-client';
	import { setControlClient } from '$lib/control-context';
	import type {
		TimelapseDecodedWindow,
		TimelapsePlanResponse,
		TimelapsePlanSample
	} from '$lib/timelapse-review';
	import type { CameraListItem, RecordingEvent, RecordingSegment } from '$lib/types';
	import PlayIcon from '@lucide/svelte/icons/play';

	type KeepMode = 'timeline' | 'stories' | 'swimlanes' | 'export' | 'timelapse';

	const baseDate = '2026-08-18';
	const dayStartMs = Date.parse(`${baseDate}T00:00:00Z`);
	const nowMs = Date.parse(`${baseDate}T06:51:44Z`);
	const at = (time: string) => Date.parse(`${baseDate}T${time}Z`);

	function svgDataUri(label: string, accent: string, timeLabel: string): string {
		const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180">
			<rect width="320" height="180" fill="#151b26"/>
			<rect y="112" width="320" height="68" fill="#1f2735"/>
			<rect x="28" y="34" width="124" height="86" rx="4" fill="#273244" stroke="#3c4b64" stroke-width="2"/>
			<rect x="190" y="76" width="98" height="48" rx="8" fill="${accent}" fill-opacity="0.28" stroke="${accent}" stroke-width="2"/>
			<rect x="12" y="12" width="136" height="22" rx="3" fill="#090c12" fill-opacity="0.8"/>
			<text x="20" y="27" fill="#e2e8f0" font-family="monospace" font-size="11">${timeLabel} UTC</text>
			<text x="239" y="105" text-anchor="middle" fill="#f8fafc" font-family="monospace" font-size="12" font-weight="700">${label}</text>
		</svg>`;
		return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
	}

	async function encodeTimelapsePreviewFrame(
		sample: TimelapsePlanSample,
		signal?: AbortSignal
	): Promise<TimelapseDecodedWindow> {
		signal?.throwIfAborted();
		const width = 320;
		const height = 180;
		const timeText = new Date(sample.actualTimeMs).toISOString().slice(11, 19);
		const canvas = document.createElement('canvas');
		canvas.width = width;
		canvas.height = height;
		const ctx = canvas.getContext('2d');
		if (ctx) {
			const skyShift = (sample.index % 6) * 8;
			ctx.fillStyle = `rgb(${22 + skyShift}, ${32 + skyShift}, ${48 + skyShift})`;
			ctx.fillRect(0, 0, width, 108);
			ctx.fillStyle = '#1f2633';
			ctx.fillRect(0, 108, width, 72);
			ctx.fillStyle = '#334155';
			ctx.fillRect(28, 36, 110, 74);
			ctx.fillStyle = '#3b82f6';
			ctx.fillRect(150 + sample.index * 16, 86, 68, 34);
			ctx.fillStyle = 'rgba(9, 12, 18, 0.8)';
			ctx.fillRect(10, 10, 168, 22);
			ctx.fillStyle = '#f8fafc';
			ctx.font = '12px monospace';
			ctx.fillText(`2026-08-18 ${timeText} UTC`, 16, 25);
		}
		if (typeof VideoEncoder === 'undefined' || typeof VideoFrame === 'undefined') {
			throw new Error('Preview encoder unavailable');
		}
		const codecs = [
			{
				encoder: { codec: 'avc1.42001e', width, height, bitrate: 300_000, framerate: 10 },
				avc: { format: 'avc' as const }
			},
			{
				encoder: { codec: 'vp8', width, height, bitrate: 300_000, framerate: 10 },
				avc: undefined
			}
		];
		for (const candidate of codecs) {
			const config: VideoEncoderConfig = candidate.avc
				? { ...candidate.encoder, avc: candidate.avc }
				: candidate.encoder;
			const support = await VideoEncoder.isConfigSupported(config);
			if (!support.supported) continue;
			let payload: Uint8Array | null = null;
			let description = new Uint8Array();
			const encoder = new VideoEncoder({
				output(chunk, metadata) {
					const bytes = new Uint8Array(chunk.byteLength);
					chunk.copyTo(bytes);
					payload = bytes;
					const desc = metadata?.decoderConfig?.description;
					if (desc instanceof ArrayBuffer) description = new Uint8Array(desc);
					else if (ArrayBuffer.isView(desc)) {
						description = new Uint8Array(desc.buffer, desc.byteOffset, desc.byteLength);
					}
				},
				error() {}
			});
			const frame = new VideoFrame(canvas, { timestamp: 0 });
			try {
				encoder.configure(config);
				encoder.encode(frame, { keyFrame: true });
				await encoder.flush();
			} finally {
				frame.close();
				if (encoder.state !== 'closed') encoder.close();
			}
			if (payload) {
				return {
					tokenKey: sample.tokenKey,
					actualTimeMs: sample.actualTimeMs,
					targetPresentationRank: 0,
					codec: candidate.encoder.codec,
					width,
					height,
					decoderConfig: description,
					samples: [
						{
							decodeIndex: 0,
							presentationRank: 0,
							randomAccess: true,
							payload
						}
					]
				};
			}
		}
		throw new Error('No supported preview codec');
	}

	const checksum = 'a41f9c2e81b40d5c3e9a7b2f6c1d8e0a4f5b6c7d8e9f0a1b2c3d4e5f7d1304b8';
	const timelapseSamples: TimelapsePlanSample[] = [
		'05:52:00',
		'06:00:00',
		'06:12:00',
		'06:28:00',
		'06:44:00'
	].map((time, index) => ({
		kind: 'sample',
		index,
		requestedTimeMs: at(time),
		actualTimeMs: at(time),
		recordingId: `rec-front-${index + 1}`,
		fragmentSequence: BigInt(index + 1),
		sampleOrdinal: 1,
		codec: 'H.264',
		width: 1920,
		height: 1080,
		encodedBytes: 38_400,
		sampleToken: new Uint8Array([index + 1, 7, 11, 13]),
		tokenKey: `tl-sample-${index + 1}`
	}));

	let mode = $state<KeepMode>('timeline');
	let selectedDate = $state(baseDate);
	let playheadMs = $state(at('06:37:23'));
	let selectedEventLabel = $state('Person · 0.91 · camera');
	let timelineEventSelected = $state(false);
	let exportReachedReady = $state(false);
	let timelapseGenerated = $state(false);

	let demoComplete = $derived(timelineEventSelected && exportReachedReady && timelapseGenerated);

	class KeepDemoControlClient extends ControlClient {
		#jobs: MediaExportJob[] = [];
		#pollCounts = new Map<string, number>();

		override async listExports(): Promise<MediaExportJob[]> {
			return [...this.#jobs];
		}

		override async createExport(
			options: Parameters<ControlClient['createExport']>[0]
		): Promise<MediaExportJob> {
			if (options.timelapse) {
				const readyTimelapse: MediaExportJob = {
					id: 'timelapse-demo-1',
					sourceId: options.sourceId,
					streamId: options.streamId,
					requestedStartMs: options.startMs,
					requestedEndMs: options.endMs,
					alignedStartMs: options.startMs,
					status: 'ready',
					progress: 1,
					bytesWritten: 4_380_000,
					estimatedBytes: 4_400_000,
					fileName: 'front-door_2026-08-18_timelapse_silent.mp4',
					sha256: checksum,
					expiresAtMs: Date.now() + 23 * 3_600_000,
					missingRanges: [{ startMs: at('06:08:00'), endMs: at('06:12:00') }],
					error: null,
					retryable: false,
					burnInTimestamp: false,
					eventSeed: null,
					timelapse: {
						sampleCount: timelapseSamples.length,
						gapCount: 1,
						outputDurationMs: 2_400,
						outputWidth: options.timelapse.maxWidth,
						outputHeight: options.timelapse.maxHeight,
						silent: true,
						sourceFingerprint: 'fp-front-door-20260818',
						siteTimezone: 'UTC',
						manifestSchemaVersion: 1,
						manifestFileName: 'front-door_2026-08-18_timelapse.manifest.json',
						manifestSha256: checksum,
						manifestBytes: 2_140
					}
				};
				this.#jobs = [readyTimelapse, ...this.#jobs.filter((job) => job.id !== readyTimelapse.id)];
				timelapseGenerated = true;
				return readyTimelapse;
			}

			const runningJob: MediaExportJob = {
				id: 'export-demo-1',
				sourceId: options.sourceId,
				streamId: options.streamId,
				requestedStartMs: options.startMs,
				requestedEndMs: options.endMs,
				alignedStartMs: options.startMs,
				status: 'running',
				progress: 0.45,
				bytesWritten: 53_100_000,
				estimatedBytes: 118_000_000,
				fileName: null,
				sha256: null,
				expiresAtMs: null,
				missingRanges: [],
				error: null,
				retryable: false,
				burnInTimestamp: options.burnInTimestamp ?? false,
				eventSeed: null
			};
			this.#pollCounts.set(runningJob.id, 0);
			this.#jobs = [runningJob, ...this.#jobs.filter((job) => job.id !== runningJob.id)];
			return runningJob;
		}

		override async getExport(jobId: string): Promise<MediaExportJob> {
			const existing = this.#jobs.find((job) => job.id === jobId);
			if (!existing) throw new Error('Export job not found.');
			const count = (this.#pollCounts.get(jobId) ?? 0) + 1;
			this.#pollCounts.set(jobId, count);
			if (count === 1) {
				const midJob: MediaExportJob = {
					...existing,
					status: 'running',
					progress: 0.82,
					bytesWritten: 96_760_000
				};
				this.#jobs = this.#jobs.map((job) => (job.id === jobId ? midJob : job));
				return midJob;
			}
			const readyJob: MediaExportJob = {
				...existing,
				status: 'ready',
				progress: 1,
				bytesWritten: 118_000_000,
				fileName: 'front-door_2026-08-18T06-11-48Z_120s.mp4',
				sha256: checksum,
				expiresAtMs: Date.now() + (23 * 60 + 42) * 60_000
			};
			this.#jobs = this.#jobs.map((job) => (job.id === jobId ? readyJob : job));
			exportReachedReady = true;
			return readyJob;
		}

		override async planTimelapse(
			options: Parameters<ControlClient['planTimelapse']>[0]
		): Promise<TimelapsePlanResponse> {
			return {
				sourceId: options.sourceId,
				streamId: options.streamId,
				requestedStartMs: at('05:51:44'),
				requestedEndMs: at('06:51:44'),
				sourceFingerprint: 'fp-front-door-20260818',
				catalogRevision: 42n,
				estimate: {
					sampleCount: timelapseSamples.length,
					gapCount: 1,
					gapDurationMs: 4 * 60_000,
					outputDurationMs: 2_400,
					estimatedSourceBytes: 18_400_000,
					estimatedOutputBytes: 4_380_000,
					requiresConfirmation: false,
					encoderAvailable: true,
					siteTimezone: 'UTC'
				},
				items: [
					timelapseSamples[0]!,
					timelapseSamples[1]!,
					{ kind: 'gap', startMs: at('06:08:00'), endMs: at('06:12:00'), reason: 'missing' },
					timelapseSamples[2]!,
					timelapseSamples[3]!,
					timelapseSamples[4]!
				],
				totalSamples: timelapseSamples.length,
				nextPageToken: ''
			};
		}

		override async fetchTimelapseSampleWindow(
			options: Parameters<ControlClient['fetchTimelapseSampleWindow']>[0]
		): Promise<TimelapseDecodedWindow> {
			const match =
				timelapseSamples.find((candidate) => candidate.sampleToken[0] === options.sampleToken[0]) ??
				timelapseSamples[0]!;
			return encodeTimelapsePreviewFrame(match, options.signal);
		}
	}

	setControlClient(new KeepDemoControlClient());
	setCapabilityState([
		'keeppeek.media-export.v1',
		'keeppeek.timelapse.v1',
		'keeppeek.timelapse-export.v1'
	]);

	const segments: RecordingSegment[] = [
		['03:40:00', '04:35:00'],
		['04:42:00', '05:25:00'],
		['05:32:00', '06:08:00'],
		['06:12:00', '06:51:44']
	].map(([start, end], index) => ({
		stream: 'main',
		date: baseDate,
		hour: start.slice(0, 2),
		filename: `front-door-${index}.mp4`,
		url: `indexed:front-door:main:${index}`,
		start_time_ms: at(start),
		end_time_ms: at(end),
		duration_ms: at(end) - at(start)
	}));

	const timelineEvents: RecordingEvent[] = [
		{
			id: 'person-now',
			source: 'camera',
			kind: 'person',
			start_time_ms: at('06:37:23'),
			end_time_ms: at('06:41:23'),
			confidence: 0.91,
			bbox: null,
			zone: 'porch',
			thumbnail_url: svgDataUri('PERSON', '#3b82f6', '06:37:23')
		},
		{
			id: 'delivery',
			source: 'camera',
			kind: 'vehicle',
			start_time_ms: at('05:45:04'),
			end_time_ms: at('05:49:04'),
			confidence: 0.84,
			bbox: null,
			zone: 'driveway',
			thumbnail_url: svgDataUri('VEHICLE', '#22c55e', '05:45:04')
		},
		{
			id: 'motion',
			source: 'camera',
			kind: 'motion',
			start_time_ms: at('04:52:51'),
			end_time_ms: at('04:56:51'),
			confidence: null,
			bbox: null,
			zone: null,
			thumbnail_url: null
		},
		{
			id: 'story-morning',
			source: 'keeppeek',
			kind: 'story',
			start_time_ms: at('04:05:40'),
			end_time_ms: at('04:07:40'),
			confidence: null,
			bbox: null,
			zone: 'porch',
			thumbnail_url: svgDataUri('STORY', '#a855f7', '04:05:40')
		}
	];

	const dates = [
		'2026-08-18',
		'2026-08-17',
		'2026-08-16',
		'2026-08-15',
		'2026-08-13',
		'2026-08-12',
		'2026-08-11'
	];

	const exportSegment: RecordingSegment = {
		stream: 'main',
		date: baseDate,
		hour: '06',
		filename: 'front-door-06.mp4',
		url: 'indexed:front-door:main:4',
		start_time_ms: at('06:11:48'),
		end_time_ms: at('06:13:48'),
		duration_ms: 120_000
	};

	const cameras: CameraListItem[] = [
		['front-door', 'Front Door'],
		['driveway', 'Driveway'],
		['side-gate', 'Side Gate'],
		['back-yard', 'Back Yard']
	].map(([id, name], index) => ({
		id,
		ip: `192.0.2.${index + 51}`,
		name,
		manufacturer: 'Reolink',
		model: `RLC-81${index + 1}A`,
		firmware_version: null,
		is_reolink: true,
		profiles: []
	}));

	function laneSegment(cameraId: string, start: string, end: string): RecordingSegment {
		return {
			stream: 'main',
			date: baseDate,
			hour: '06',
			filename: `${cameraId}-${start}.mp4`,
			url: `indexed:${cameraId}:main:${start}`,
			start_time_ms: at(start),
			end_time_ms: at(end),
			duration_ms: at(end) - at(start)
		};
	}

	const lanes = [
		{
			camera: cameras[0]!,
			segments: [laneSegment('front-door', '06:00:00', '07:00:00')],
			events: [
				{
					id: 'front-person',
					source: 'camera' as const,
					kind: 'person',
					start_time_ms: at('06:13:12'),
					end_time_ms: at('06:18:36'),
					confidence: 0.91,
					bbox: null,
					zone: 'porch',
					thumbnail_url: null
				}
			]
		},
		{
			camera: cameras[1]!,
			segments: [laneSegment('driveway', '06:00:00', '07:00:00')],
			events: [
				{
					id: 'driveway-vehicle',
					source: 'camera' as const,
					kind: 'vehicle',
					start_time_ms: at('06:12:04'),
					end_time_ms: at('06:16:04'),
					confidence: 0.84,
					bbox: null,
					zone: 'driveway',
					thumbnail_url: null
				}
			]
		},
		{
			camera: cameras[2]!,
			segments: [laneSegment('side-gate', '06:00:00', '07:00:00')],
			events: [
				{
					id: 'side-motion',
					source: 'camera' as const,
					kind: 'motion',
					start_time_ms: at('06:36:00'),
					end_time_ms: at('06:39:36'),
					confidence: null,
					bbox: null,
					zone: null,
					thumbnail_url: null
				}
			]
		},
		{
			camera: cameras[3]!,
			segments: [
				laneSegment('back-yard', '06:00:00', '06:21:36'),
				laneSegment('back-yard', '06:30:00', '07:00:00')
			],
			events: []
		}
	];

	let formattedPlayhead = $derived(
		new Date(playheadMs).toISOString().replace('T', ' ').slice(0, 19)
	);
	let activeSegmentUrl = $derived(
		segments.find(
			(segment) => playheadMs >= segment.start_time_ms && playheadMs <= segment.end_time_ms
		)?.url ?? segments.at(-1)!.url
	);

	function handleSeek(timestampMs: number): void {
		playheadMs = timestampMs;
	}

	function handleEventPreview(event: RecordingEvent): void {
		timelineEventSelected = true;
		const label = event.kind.charAt(0).toUpperCase() + event.kind.slice(1);
		selectedEventLabel = `${label}${event.confidence === null ? '' : ` · ${event.confidence.toFixed(2)}`} · ${event.source}`;
	}

	const modes: readonly { id: KeepMode; label: string }[] = [
		{ id: 'timeline', label: 'Timeline' },
		{ id: 'stories', label: 'Stories' },
		{ id: 'swimlanes', label: 'Swimlanes' },
		{ id: 'export', label: 'Export' },
		{ id: 'timelapse', label: 'Timelapse' }
	];
</script>

<main
	data-paper-scenario="keep.desktop.timeline-anatomy"
	data-demo-keep-timeline
	data-demo-keep-complete={demoComplete ? 'true' : undefined}
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="keep" paperFull />

	<section class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header
			class="flex h-14 shrink-0 items-center justify-between gap-4 border-b border-hairline bg-surface px-5"
		>
			<div class="flex items-center gap-3">
				<h1 class="text-sm font-semibold">Keep · Front Door</h1>
				<span class="rounded-sm border border-hairline bg-raised px-2 py-0.5 font-mono text-2xs">
					{selectedDate} UTC
				</span>
				<span class="font-mono text-2xs text-text-muted tabular-nums">
					{formattedPlayhead} UTC
				</span>
			</div>

			<nav class="flex items-center gap-1 rounded-sm bg-raised p-1" aria-label="Keep modes">
				{#each modes as item (item.id)}
					<button
						type="button"
						data-demo-action={`mode-${item.id}`}
						aria-pressed={mode === item.id}
						class="h-8 rounded-xs px-3 text-xs font-medium transition-colors {mode === item.id
							? 'bg-primary text-on-primary'
							: 'text-text-muted hover:text-foreground'}"
						onclick={() => (mode = item.id)}
					>
						{item.label}
					</button>
				{/each}
			</nav>
		</header>

		<div class="min-h-0 flex-1 overflow-hidden p-4">
			{#if mode === 'timeline'}
				<div class="grid h-full grid-cols-[minmax(0,1fr)_396px] gap-4">
					<section
						class="flex min-h-0 flex-col overflow-hidden rounded-md border border-hairline bg-video"
						aria-label="Recorded playback surface"
					>
						<div class="relative flex min-h-0 flex-1 flex-col justify-between p-4">
							<div class="flex items-start justify-between gap-4">
								<div class="rounded-sm bg-black/75 px-3 py-2 font-mono text-white">
									<p class="text-xs font-semibold tabular-nums">{formattedPlayhead}.412 UTC</p>
									<p class="mt-1 text-2xs text-white/65">
										MAIN · 3840×2160 · 25 FPS · H.264 · {selectedEventLabel}
									</p>
								</div>
								<span
									class="rounded-full bg-black/75 px-2.5 py-1 font-mono text-2xs tracking-caps text-healthy"
								>
									INDEXED KEYFRAME PLAYBACK
								</span>
							</div>

							<svg
								viewBox="0 0 960 440"
								class="mx-auto max-h-[420px] w-full"
								role="img"
								aria-label={`Recorded Front Door frame at ${formattedPlayhead} UTC`}
							>
								<rect width="960" height="250" fill="#182232" />
								<rect y="250" width="960" height="190" fill="#1e2530" />
								<polygon points="270,250 740,250 920,440 80,440" fill="#27303f" />
								<rect
									x="60"
									y="90"
									width="270"
									height="170"
									rx="4"
									fill="#2a3444"
									stroke="#475569"
									stroke-width="2"
								/>
								<rect
									x="170"
									y="140"
									width="64"
									height="120"
									rx="2"
									fill="#3c2f2a"
									stroke="#64748b"
								/>
								{#if timelineEventSelected}
									<g transform="translate(500, 210)">
										<rect
											width="230"
											height="118"
											rx="14"
											fill="#dbeafe"
											stroke="#22c55e"
											stroke-width="3"
										/>
										<rect x="24" y="18" width="182" height="44" rx="6" fill="#1e293b" />
										<rect x="78" y="80" width="74" height="22" rx="3" fill="#ffffff" />
										<text
											x="115"
											y="95"
											text-anchor="middle"
											fill="#0f172a"
											font-family="monospace"
											font-size="12"
											font-weight="700"
										>
											KP-4829
										</text>
									</g>
									<rect
										x="492"
										y="202"
										width="246"
										height="134"
										fill="none"
										stroke="#22c55e"
										stroke-width="2"
										stroke-dasharray="6 4"
									/>
									<rect x="492" y="180" width="128" height="22" rx="3" fill="#22c55e" />
									<text
										x="556"
										y="195"
										text-anchor="middle"
										fill="#052e16"
										font-family="monospace"
										font-size="11"
										font-weight="700"
									>
										vehicle 0.84
									</text>
								{:else}
									<g transform="translate(360, 200)">
										<circle cx="48" cy="24" r="16" fill="#93c5fd" />
										<rect x="28" y="44" width="40" height="72" rx="10" fill="#3b82f6" />
									</g>
									<rect
										x="348"
										y="192"
										width="120"
										height="142"
										fill="none"
										stroke="#3b82f6"
										stroke-width="2"
									/>
									<rect x="348" y="170" width="108" height="22" rx="3" fill="#3b82f6" />
									<text
										x="402"
										y="185"
										text-anchor="middle"
										fill="#ffffff"
										font-family="monospace"
										font-size="11"
										font-weight="700"
									>
										person 0.91
									</text>
								{/if}
							</svg>

							<div
								class="flex h-10 items-center gap-3 rounded-sm bg-black/75 px-3 font-mono text-2xs text-white/80"
							>
								<PlayIcon class="size-4 text-white" fill="currentColor" />
								<span>Prev frame</span>
								<span>Next frame</span>
								<span class="h-1 flex-1 rounded-full bg-white/20">
									<span class="block h-full w-2/3 rounded-full bg-primary"></span>
								</span>
								<span>{formattedPlayhead.slice(11)} / 24:00:00 UTC</span>
							</div>
						</div>
					</section>

					<div class="min-h-0 overflow-hidden">
						<VerticalTimeline
							{segments}
							events={timelineEvents}
							selectedUrl={activeSegmentUrl}
							{playheadMs}
							{dayStartMs}
							{nowMs}
							onSeek={handleSeek}
							onEventPreview={handleEventPreview}
						/>
					</div>
				</div>
			{:else if mode === 'stories'}
				<div class="mx-auto max-w-5xl pt-2">
					<KeepStories
						events={timelineEvents}
						{dates}
						{selectedDate}
						ondate={(nextDate) => (selectedDate = nextDate)}
						onseek={(timestampMs) => {
							playheadMs = timestampMs;
							mode = 'timeline';
						}}
						panel="all"
					/>
				</div>
			{:else if mode === 'swimlanes'}
				<div class="mx-auto max-w-6xl pt-2">
					<KeepSwimlanes
						{cameras}
						selectedCameraId="front-door"
						date={selectedDate}
						anchorMs={at('06:59:00')}
						{playheadMs}
						{lanes}
						onselect={(_cameraId, timestampMs) => {
							playheadMs = timestampMs;
						}}
					/>
				</div>
			{:else if mode === 'export'}
				<div class="mx-auto max-w-2xl pt-2">
					<KeepExportPanel
						sourceId="front-door"
						sourceName="Front Door"
						segment={exportSegment}
						bitrateKbps={6_200}
					/>
				</div>
			{:else}
				<div class="h-full overflow-y-auto pr-1">
					<TimelapseReview
						sourceId="front-door"
						sourceName="Front Door"
						streamId="main"
						anchorMs={nowMs}
						onseek={(timestampMs) => {
							playheadMs = timestampMs;
							mode = 'timeline';
						}}
					/>
				</div>
			{/if}
		</div>
	</section>
</main>
