<script lang="ts">
	import { onMount } from 'svelte';
	import CameraOverview from '$lib/components/CameraOverview.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import { ControlClient, type PtzPreset } from '$lib/control-client';
	import { setControlClient } from '$lib/control-context';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type { CameraHealth, CameraListItem, MotionDetection } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import MicIcon from '@lucide/svelte/icons/mic';
	import MicOffIcon from '@lucide/svelte/icons/mic-off';
	import ShieldCheckIcon from '@lucide/svelte/icons/shield-check';
	import Volume2Icon from '@lucide/svelte/icons/volume-2';

	const presets: PtzPreset[] = [
		{ id: 1, name: 'Front step' },
		{ id: 2, name: 'Driveway gate' },
		{ id: 3, name: 'Side path' }
	];

	const camera: CameraListItem = {
		id: 'front-door',
		ip: '192.168.1.42',
		name: 'Front Door',
		manufacturer: 'Reolink',
		model: 'RLC-811A',
		firmware_version: 'v3.1.0',
		serial_number: 'REO-0001',
		hardware_id: 'RLC-811A',
		hostname: 'front-door',
		mac_address: '02:00:00:00:00:42',
		is_reolink: true,
		backend: 'reo-proto',
		transport: 'tcp',
		ports: { http: 80, https: null, rtsp: 554, onvif: 8000 },
		capabilities: {
			ptz: true,
			audio: true,
			events: true,
			recording: true,
			analytics: false,
			imaging: true,
			two_way_audio: true
		},
		profiles: [
			{
				name: 'mainStream',
				stream: 'main',
				encoding: 'h265',
				resolution: '3840x2160',
				framerate: 25,
				bitrate_kbps: 8_192,
				gop: 50,
				h264_profile: null,
				audio: { encoding: 'aac', sample_rate: 16_000, bitrate_kbps: 64 }
			},
			{
				name: 'subStream',
				stream: 'sub',
				encoding: 'h264',
				resolution: '640x360',
				framerate: 15,
				bitrate_kbps: 600,
				gop: 30,
				h264_profile: 'baseline',
				audio: null
			}
		]
	};

	const health: CameraHealth = {
		id: camera.id,
		ip: camera.ip,
		name: camera.name ?? camera.id,
		manufacturer: camera.manufacturer,
		model: camera.model,
		firmware_version: camera.firmware_version,
		backend: camera.backend,
		transport: camera.transport,
		state: 'healthy',
		lifecycle: 'Connected',
		last_error: null,
		configured_profiles: camera.profiles,
		streams: [
			{
				type: 'video_main',
				codec: 'h265',
				resolution: '3840x2160',
				fps: 25,
				expected_fps: 25,
				kbps: 8_192,
				frames: 12_500,
				drops: 0,
				reconnects: 0,
				updated_at_ms: Date.parse('2026-08-18T08:42:10Z'),
				report_age_ms: 12
			},
			{
				type: 'video_sub',
				codec: 'h264',
				resolution: '640x360',
				fps: 15,
				expected_fps: 15,
				kbps: 600,
				frames: 7_500,
				drops: 0,
				reconnects: 0,
				updated_at_ms: Date.parse('2026-08-18T08:42:10Z'),
				report_age_ms: 12
			}
		]
	};

	const sectionAnchors = [
		'Overview',
		'Configuration',
		'Privacy',
		'Connection',
		'Events',
		'Streams',
		'Audio',
		'Advanced'
	] as const;

	let panOffset = $state(0);
	let tiltOffset = $state(0);
	let zoomFactor = $state(1);
	let targetPan = 0;
	let targetTilt = 0;
	let targetZoom = 1;
	let activeVelocity = { pan: 0, tilt: 0, zoom: 0 };

	let ptzStatus = $state('Idle · WebRTC control ready');
	let activePresetName = $state<string | null>('Front step');
	let ptzStopped = $state(false);
	let presetRecalled = $state(false);

	let motionDetection = $state.raw<MotionDetection>({
		supported: true,
		controllable: true,
		enabled: false,
		error: null
	});
	let updatingMotion = $state(false);
	let motionVerifiedReadback = $state(false);
	let motionTimer: ReturnType<typeof setTimeout> | null = null;

	let talkbackActive = $state(false);
	let talkbackConfirmed = $state(false);

	let demoCompleted = $derived(
		ptzStopped && presetRecalled && motionVerifiedReadback && talkbackConfirmed
	);

	class DemoCameraControlClient extends ControlClient {
		override async getPtzPresets(): Promise<PtzPreset[]> {
			return presets;
		}

		override async movePtz(
			_sourceId: string,
			movement: { pan: number; tilt: number; zoom: number }
		): Promise<void> {
			activeVelocity = { ...movement };
			activePresetName = null;
			const parts = [
				movement.pan !== 0 ? `pan ${movement.pan > 0 ? '+1' : '-1'}` : null,
				movement.tilt !== 0 ? `tilt ${movement.tilt > 0 ? '+1' : '-1'}` : null,
				movement.zoom !== 0 ? `zoom ${movement.zoom > 0 ? 'in' : 'out'}` : null
			].filter(Boolean);
			ptzStatus = `Continuous move · ${parts.join(', ')}`;
		}

		override async stopPtz(): Promise<void> {
			activeVelocity = { pan: 0, tilt: 0, zoom: 0 };
			targetPan = panOffset;
			targetTilt = tiltOffset;
			targetZoom = zoomFactor;
			ptzStopped = true;
			ptzStatus = 'Stopped · camera idle confirmed';
		}

		override async gotoPtzPreset(_sourceId: string, presetId: number): Promise<void> {
			activeVelocity = { pan: 0, tilt: 0, zoom: 0 };
			const preset = presets.find((candidate) => candidate.id === presetId);
			if (presetId === 1) {
				targetPan = 0;
				targetTilt = 0;
				targetZoom = 1;
			} else if (presetId === 2) {
				targetPan = 42;
				targetTilt = -6;
				targetZoom = 1.24;
			} else {
				targetPan = -36;
				targetTilt = 4;
				targetZoom = 1.12;
			}
			activePresetName = preset?.name ?? `Preset ${presetId}`;
			presetRecalled = true;
			ptzStatus = `Preset ${presetId} recalled · ${activePresetName}`;
		}

		override async setMotionDetection(
			_sourceId: string,
			enabled: boolean
		): Promise<MotionDetection> {
			return new Promise((resolveResult) => {
				if (motionTimer !== null) clearTimeout(motionTimer);
				motionTimer = setTimeout(() => {
					motionTimer = null;
					resolveResult({
						supported: true,
						controllable: true,
						enabled,
						error: null
					});
				}, 260);
			});
		}
	}

	const controlClient = new DemoCameraControlClient();
	setControlClient(controlClient);
	const livePeer = setLivePeer();

	function drawCameraBackdrop(
		context: CanvasRenderingContext2D,
		width: number,
		height: number
	): void {
		const sky = context.createLinearGradient(0, 0, 0, height * 0.55);
		sky.addColorStop(0, '#121e2e');
		sky.addColorStop(1, '#1c2b3d');
		context.fillStyle = sky;
		context.fillRect(-140, -60, width + 280, height * 0.62);

		context.fillStyle = '#18212b';
		context.fillRect(-140, height * 0.48, width + 280, height * 0.65);

		context.fillStyle = '#232e3c';
		context.beginPath();
		context.moveTo(180, height * 0.48);
		context.lineTo(540, height * 0.48);
		context.lineTo(680, height + 40);
		context.lineTo(60, height + 40);
		context.closePath();
		context.fill();

		context.strokeStyle = '#334559';
		context.lineWidth = 2;
		context.beginPath();
		context.moveTo(360, height * 0.5);
		context.lineTo(360, height);
		context.stroke();

		context.fillStyle = '#263445';
		context.fillRect(58, 128, 174, 108);
		context.strokeStyle = '#3f5670';
		context.strokeRect(58, 128, 174, 108);
		context.fillStyle = '#d99b38';
		context.globalAlpha = 0.24;
		context.fillRect(92, 152, 46, 54);
		context.globalAlpha = 1;

		context.strokeStyle = '#536c88';
		context.lineWidth = 3;
		context.strokeRect(520, 148, 178, 86);
		for (let bar = 540; bar < 695; bar += 22) {
			context.beginPath();
			context.moveTo(bar, 148);
			context.lineTo(bar, 234);
			context.stroke();
		}
		context.fillStyle = '#9bb8d6';
		context.font = '600 11px monospace';
		context.fillText('DRIVEWAY GATE', 552, 138);

		context.strokeStyle = 'rgba(94, 180, 255, 0.25)';
		context.lineWidth = 1;
		context.setLineDash([5, 5]);
		context.strokeRect(190, 185, 340, 160);
		context.setLineDash([]);
	}

	function drawCameraHud(
		context: CanvasRenderingContext2D,
		width: number,
		height: number,
		tick: number
	): void {
		const second = Math.floor(tick / 15) % 60;
		const timeStamp = `2026-08-18 08:42:${(10 + second).toString().padStart(2, '0')} UTC`;
		context.fillStyle = 'rgba(9, 13, 18, 0.78)';
		context.fillRect(14, height - 38, 305, 24);
		context.fillStyle = '#d6e2ef';
		context.font = '500 12px monospace';
		context.fillText(
			`${timeStamp} · PAN ${Math.round(panOffset)}° · ${zoomFactor.toFixed(2)}x`,
			22,
			height - 22
		);

		if (talkbackActive) {
			context.fillStyle = 'rgba(16, 185, 129, 0.18)';
			context.fillRect(width - 210, height - 42, 196, 28);
			context.fillStyle = '#6ee7b7';
			context.font = '600 11px monospace';
			context.fillText('TALKBACK TX · 16 KHZ ADPCM', width - 198, height - 24);
			for (let bar = 0; bar < 8; bar += 1) {
				const amplitude = 4 + ((tick * 3 + bar * 5) % 14);
				context.fillRect(width - 208 + bar * 6, height - 44 - amplitude, 3, amplitude);
			}
		}
	}

	function drawCameraScene(
		context: CanvasRenderingContext2D,
		width: number,
		height: number,
		tick: number
	): void {
		context.fillStyle = '#0c1118';
		context.fillRect(0, 0, width, height);

		context.save();
		context.translate(width / 2, height / 2);
		context.scale(zoomFactor, zoomFactor);
		context.translate(-width / 2 - panOffset * 2.2, -height / 2 + tiltOffset * 2.2);
		drawCameraBackdrop(context, width, height);
		context.restore();

		drawCameraHud(context, width, height, tick);
	}

	onMount(() => {
		const canvas = document.createElement('canvas');
		canvas.width = 720;
		canvas.height = 405;
		const context = canvas.getContext('2d');
		let frameTick = 0;
		let stream: MediaStream | null = null;

		const renderFrame = () => {
			if (activeVelocity.pan !== 0) {
				panOffset = Math.max(-50, Math.min(50, panOffset + activeVelocity.pan * 2.4));
				targetPan = panOffset;
			} else {
				panOffset += (targetPan - panOffset) * 0.22;
			}
			if (activeVelocity.tilt !== 0) {
				tiltOffset = Math.max(-25, Math.min(25, tiltOffset + activeVelocity.tilt * 1.4));
				targetTilt = tiltOffset;
			} else {
				tiltOffset += (targetTilt - tiltOffset) * 0.22;
			}
			zoomFactor += (targetZoom - zoomFactor) * 0.22;
			if (context) drawCameraScene(context, canvas.width, canvas.height, frameTick);
			frameTick += 1;
		};

		renderFrame();
		if (typeof canvas.captureStream === 'function') {
			stream = canvas.captureStream(15);
			livePeer.tracks = {
				[camera.id]: {
					cameraId: camera.id,
					trackId: 'demo-front-door-video',
					audioTrackId: 'demo-front-door-audio',
					receiver: null,
					stream,
					audioReceiver: null,
					audioStream: null,
					status: 'live',
					requestedQuality: 'auto',
					requestedVariantId: 'sub',
					activeStream: 'sub',
					pendingStream: null,
					estimatedBitrateBps: 8_792_000,
					subscribed: true
				}
			};
		}

		const timer = window.setInterval(renderFrame, 66);
		return () => {
			window.clearInterval(timer);
			if (motionTimer !== null) {
				clearTimeout(motionTimer);
				motionTimer = null;
			}
			for (const track of stream?.getTracks() ?? []) track.stop();
			livePeer.tracks = {};
		};
	});

	async function handleMotionToggle(event: Event): Promise<void> {
		const enabled = (event.currentTarget as HTMLInputElement).checked;
		if (updatingMotion) return;
		updatingMotion = true;
		try {
			motionDetection = await controlClient.setMotionDetection(camera.id, enabled);
			motionVerifiedReadback = true;
		} finally {
			updatingMotion = false;
		}
	}

	function toggleTalkback(): void {
		talkbackActive = !talkbackActive;
		if (talkbackActive) talkbackConfirmed = true;
	}
</script>

<main
	data-paper-scenario="camera.desktop.details-ptz"
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="cameras" />

	<div class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header class="flex h-12 shrink-0 items-center gap-3 border-b border-hairline bg-surface px-5">
			<span class="text-xs text-text-muted">Cameras</span>
			<ChevronRightIcon class="size-3 text-text-faint" />
			<h1 class="text-base font-semibold">{camera.name}</h1>
			<span
				class="inline-flex h-6 items-center gap-1.5 rounded-full border border-hairline bg-raised px-2.5 text-xs"
			>
				<span class="size-1.5 rounded-full bg-healthy"></span>Healthy
			</span>
			<span class="font-mono text-2xs text-text-muted">
				{camera.ip} · {camera.manufacturer}
				{camera.model} · reo-proto TCP
			</span>
			<span class="flex-1"></span>
			<span class="font-mono text-2xs text-primary-soft">{ptzStatus}</span>
			{#if demoCompleted}
				<span
					data-demo-camera-controls-complete="true"
					class="inline-flex items-center gap-1.5 rounded-full border border-healthy/40 bg-healthy/10 px-2.5 py-1 font-mono text-2xs text-healthy"
				>
					<CheckIcon class="size-3" /> PTZ · MOTION · TALK VERIFIED
				</span>
			{/if}
		</header>

		<div class="flex min-h-0 flex-1 overflow-hidden">
			<aside
				class="flex w-[168px] shrink-0 flex-col border-r border-hairline bg-surface px-3 py-4"
				aria-label="Camera sections"
			>
				<p class="px-2.5 pb-2 font-mono text-[10px] tracking-caps text-text-faint">ON THIS PAGE</p>
				<nav class="space-y-0.5">
					{#each sectionAnchors as anchor, index (anchor)}
						<span
							class="flex h-8 items-center gap-2 rounded-sm px-2.5 text-xs {index === 0
								? 'bg-raised font-medium text-foreground'
								: 'text-text-muted'}"
						>
							<span class="h-3.5 w-0.5 {index === 0 ? 'bg-primary' : ''}"></span>{anchor}
						</span>
					{/each}
				</nav>

				<div class="mt-auto rounded-sm border border-hairline bg-raised p-2.5">
					<p class="font-mono text-[10px] tracking-caps text-text-faint">PTZ HEADING</p>
					<svg
						viewBox="0 0 120 72"
						class="mt-2 h-16 w-full rounded-xs bg-ground"
						role="img"
						aria-label="Camera field of view heading indicator"
					>
						<path
							d="M 16 62 A 44 44 0 0 1 104 62"
							fill="none"
							stroke="#2a3647"
							stroke-width="1.5"
						/>
						<g transform={`translate(60 62) rotate(${panOffset * 0.9})`}>
							<path d="M 0 0 L -16 -42 A 44 44 0 0 1 16 -42 Z" fill="rgba(56, 189, 248, 0.24)" />
							<line x1="0" y1="0" x2="0" y2="-44" stroke="#38bdf8" stroke-width="2" />
						</g>
						<circle cx="60" cy="62" r="3.5" fill="#e2e8f0" />
					</svg>
					<p class="mt-1.5 font-mono text-[10px] text-text-muted">
						{activePresetName ?? `Pan ${Math.round(panOffset)}°`}
					</p>
				</div>
			</aside>

			<div class="flex min-h-0 min-w-0 flex-1 flex-col gap-3.5 overflow-hidden p-4">
				<CameraOverview {camera} {health} stream="sub" previewAvailable commandTransportAvailable />

				<div class="grid min-h-0 flex-1 grid-cols-3 gap-3.5">
					<section
						class="flex flex-col justify-between rounded-md border border-hairline bg-surface p-3.5"
						aria-labelledby="profiles-summary-heading"
					>
						<div>
							<div class="flex items-center justify-between gap-2">
								<h2 id="profiles-summary-heading" class="text-sm font-semibold">Media profiles</h2>
								<span class="font-mono text-2xs text-healthy">0 DROPS</span>
							</div>
							<dl class="mt-2.5 space-y-2 text-xs">
								<div class="flex items-center justify-between rounded-sm bg-raised px-2.5 py-2">
									<dt class="font-medium">Main</dt>
									<dd class="font-mono text-2xs text-text-muted">3840×2160 · H.265 · 25 fps</dd>
								</div>
								<div class="flex items-center justify-between rounded-sm bg-raised px-2.5 py-2">
									<dt class="font-medium">Sub</dt>
									<dd class="font-mono text-2xs text-text-muted">640×360 · H.264 · 15 fps</dd>
								</div>
							</dl>
						</div>
						<p class="font-mono text-2xs text-text-faint">
							CONTROL: REOLINK HTTP CH 0 · MEDIA: REO-PROTO
						</p>
					</section>

					<section
						id="events"
						class="flex flex-col justify-between rounded-md border border-hairline bg-surface p-3.5"
						aria-labelledby="motion-heading"
					>
						<div>
							<div class="flex items-center justify-between gap-3">
								<div>
									<h2 id="motion-heading" class="text-sm font-semibold">Motion detection</h2>
									<p class="mt-0.5 text-xs text-text-muted">
										{motionDetection.enabled ? 'Enabled' : 'Disabled'}
									</p>
								</div>
								<label class="flex cursor-pointer items-center gap-2 text-xs font-medium">
									<input
										type="checkbox"
										role="switch"
										aria-label="Motion detection enabled"
										class="size-4 accent-emerald-600 disabled:cursor-not-allowed"
										checked={motionDetection.enabled ?? false}
										disabled={updatingMotion}
										onchange={(event) => void handleMotionToggle(event)}
									/>
									<span>{updatingMotion ? 'Updating' : 'Enabled'}</span>
								</label>
							</div>
							<p class="mt-2.5 text-xs leading-5 text-text-muted">
								Reads alarm config, toggles enabled while preserving masks and sensitivity, then
								verifies via read-back.
							</p>
						</div>
						<div
							class="flex items-center gap-2 rounded-sm border px-2.5 py-1.5 font-mono text-2xs {motionVerifiedReadback
								? 'border-healthy/40 bg-healthy/10 text-healthy'
								: 'border-hairline bg-raised text-text-faint'}"
							role="status"
						>
							<ShieldCheckIcon class="size-3.5 shrink-0" />
							{motionVerifiedReadback
								? 'DEVICE READ-BACK VERIFIED: ENABLED'
								: 'ADMINISTRATOR ONLY · AWAITING TOGGLE'}
						</div>
					</section>

					<section
						id="audio"
						class="flex flex-col justify-between rounded-md border border-hairline bg-surface p-3.5"
						aria-labelledby="talk-heading"
					>
						<div>
							<div class="flex items-center justify-between gap-2">
								<h2 id="talk-heading" class="text-sm font-semibold">Two-way audio (Talk)</h2>
								<span
									class="inline-flex items-center gap-1 rounded-full border border-healthy/40 bg-healthy/10 px-2 py-0.5 font-mono text-[10px] text-healthy"
								>
									<Volume2Icon class="size-3" /> SPEAKER CONFIRMED
								</span>
							</div>
							<p class="mt-2 text-xs leading-5 text-text-muted">
								Direct TCP Baichuan probe confirmed 16 kHz mono IMA ADPCM speaker. Administrator
								role required; Users keep PTZ and quick replies.
							</p>
						</div>
						<div class="flex items-center justify-between gap-2 pt-2">
							<button
								type="button"
								data-demo-action="toggle-talkback"
								class="inline-flex h-9 items-center gap-2 rounded-sm px-3.5 text-xs font-semibold focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {talkbackActive
									? 'border border-healthy/50 bg-healthy/15 text-healthy'
									: 'bg-primary text-on-primary'}"
								aria-pressed={talkbackActive}
								onclick={toggleTalkback}
							>
								{#if talkbackActive}
									<MicOffIcon class="size-3.5" /> Stop talkback
								{:else}
									<MicIcon class="size-3.5" /> Talkback
								{/if}
							</button>
							<span class="font-mono text-2xs text-text-faint">
								{talkbackActive ? 'TX ACTIVE · ADMIN' : 'ROLE: ADMINISTRATOR'}
							</span>
						</div>
					</section>
				</div>
			</div>
		</div>
	</div>
</main>
