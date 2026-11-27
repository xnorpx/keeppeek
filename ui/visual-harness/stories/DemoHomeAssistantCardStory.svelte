<script lang="ts">
	import { mount, unmount } from 'svelte';
	import Card from '$lib/home-assistant/Card.svelte';
	import Editor from '$lib/home-assistant/Editor.svelte';
	import cardStyles from '$lib/home-assistant/card.css?inline';
	import { parseCardConfig, type CardConfig, type CardSource } from '$lib/home-assistant/config';
	import {
		sourceKey,
		type SessionSnapshot,
		type SourceChoice,
		type StreamState
	} from '$lib/home-assistant/connection-manager';
	import { CardView } from '$lib/home-assistant/elements.svelte';
	import CheckIcon from '@lucide/svelte/icons/check';
	import HomeIcon from '@lucide/svelte/icons/home';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import SlidersHorizontalIcon from '@lucide/svelte/icons/sliders-horizontal';

	type CameraSpec = {
		sourceId: string;
		title: string;
		accent: string;
		skyTop: string;
		skyBottom: string;
		phaseOffset: number;
	};

	const cameraSpecs: readonly CameraSpec[] = [
		{
			sourceId: '192.0.2.20',
			title: 'Front door',
			accent: '#38bdf8',
			skyTop: '#0f172a',
			skyBottom: '#1e293b',
			phaseOffset: 0
		},
		{
			sourceId: '192.0.2.21',
			title: 'Driveway',
			accent: '#34d399',
			skyTop: '#0c1a17',
			skyBottom: '#172e29',
			phaseOffset: 2.1
		},
		{
			sourceId: '192.0.2.22',
			title: 'Back yard',
			accent: '#a78bfa',
			skyTop: '#161426',
			skyBottom: '#262040',
			phaseOffset: 4.2
		}
	];

	const discoveredCatalog: readonly SourceChoice[] = cameraSpecs.map((spec) => ({
		source_id: spec.sourceId,
		title: spec.title,
		available: true
	}));

	const initialConfig: CardConfig = {
		type: 'custom:keeppeek-card',
		endpoint: 'https://keeppeek.example.net',
		token: 'ha-demo-user-token',
		title: 'Entrances',
		sources: [
			{ source_id: '192.0.2.20', title: 'Front door', quality: 'auto' },
			{ source_id: '192.0.2.21', title: 'Driveway', quality: 'low' }
		],
		layout: 'grid',
		columns: 2,
		aspect_ratio: '16:9',
		show_name: true,
		view: 'live'
	};

	const cardView = new CardView();
	cardView.config = initialConfig;
	cardView.visible = true;

	let editorConfig = $state.raw<Record<string, unknown>>({ ...initialConfig });
	let discoveredCameras = $state.raw<readonly SourceChoice[]>([]);
	let sourcesLoaded = $state(false);
	let editorError = $state<string | null>(null);
	let sawFocusInGrid = $state(false);
	let returnedToGridAfterFocus = $state(false);

	let editorHost = $state<HTMLDivElement>();
	let cardHost = $state<HTMLDivElement>();

	function drawCameraFrame(
		context: CanvasRenderingContext2D,
		spec: CameraSpec,
		frameIndex: number
	): void {
		const width = context.canvas.width;
		const height = context.canvas.height;
		const gradient = context.createLinearGradient(0, 0, 0, height);
		gradient.addColorStop(0, spec.skyTop);
		gradient.addColorStop(1, spec.skyBottom);
		context.fillStyle = gradient;
		context.fillRect(0, 0, width, height);

		context.fillStyle = 'rgba(255, 255, 255, 0.05)';
		context.fillRect(0, height * 0.62, width, height * 0.38);

		context.strokeStyle = 'rgba(255, 255, 255, 0.08)';
		context.lineWidth = 1;
		for (let line = 1; line <= 4; line += 1) {
			const y = height * 0.62 + line * 22;
			context.beginPath();
			context.moveTo(0, y);
			context.lineTo(width, y);
			context.stroke();
		}

		const angle = frameIndex * 0.06 + spec.phaseOffset;
		const sweepX = width * (0.22 + 0.56 * ((Math.sin(angle) + 1) / 2));
		const sweepY = height * 0.68;
		const halo = context.createRadialGradient(sweepX, sweepY, 6, sweepX, sweepY, 74);
		halo.addColorStop(0, `${spec.accent}44`);
		halo.addColorStop(1, `${spec.accent}00`);
		context.fillStyle = halo;
		context.fillRect(0, 0, width, height);

		context.fillStyle = spec.accent;
		context.beginPath();
		context.arc(sweepX, sweepY - 8, 7, 0, Math.PI * 2);
		context.fill();

		context.fillStyle = 'rgba(11, 14, 17, 0.72)';
		context.fillRect(14, 14, 198, 28);
		context.fillStyle = '#edf1f3';
		context.font = '600 13px sans-serif';
		context.fillText(spec.title.toUpperCase(), 24, 33);

		const totalSeconds = Math.floor(frameIndex / 15);
		const sec = (23 + totalSeconds) % 60;
		const min = 37 + Math.floor((23 + totalSeconds) / 60);
		const timestamp = `2026-08-18 06:${String(min).padStart(2, '0')}:${String(sec).padStart(2, '0')}`;
		context.fillStyle = 'rgba(11, 14, 17, 0.72)';
		context.fillRect(width - 178, 14, 164, 28);
		context.fillStyle = '#cbd5e1';
		context.font = '500 12px monospace';
		context.fillText(timestamp, width - 168, 33);

		context.fillStyle = '#79d5a4';
		context.beginPath();
		context.arc(24, height - 22, 4, 0, Math.PI * 2);
		context.fill();
		context.fillStyle = 'rgba(237, 241, 243, 0.82)';
		context.font = '500 11px monospace';
		context.fillText('DIRECT WEBRTC · 1080P · H.264', 36, height - 18);
	}

	function attachCardShadowRoot(host: HTMLElement): ShadowRoot {
		const root = host.shadowRoot ?? host.attachShadow({ mode: 'open' });
		root.replaceChildren();
		const style = document.createElement('style');
		style.textContent = cardStyles;
		root.append(style);
		return root;
	}

	function handleConfigChange(nextConfig: Record<string, unknown>): void {
		editorConfig = nextConfig;
		try {
			cardView.config = parseCardConfig(nextConfig);
			editorError = null;
		} catch (cause) {
			editorError = cause instanceof Error ? cause.message : 'Check the card configuration.';
		}
	}

	function handleDiscoverSources(): void {
		discoveredCameras = discoveredCatalog;
		sourcesLoaded = true;
		editorError = null;
	}

	function handleDemand(sources: CardSource[]): void {
		const activeConfig = cardView.config;
		if (!activeConfig || activeConfig.layout !== 'grid') return;
		if (sources.length === 1 && activeConfig.sources.length > 1) {
			sawFocusInGrid = true;
		} else if (sawFocusInGrid && sources.length > 1) {
			returnedToGridAfterFocus = true;
		}
	}

	$effect(() => {
		const streams = new Map<string, StreamState>();
		const activeStreams: MediaStream[] = [];
		const canvases: Array<{ context: CanvasRenderingContext2D; spec: CameraSpec }> = [];

		for (const spec of cameraSpecs) {
			const canvas = document.createElement('canvas');
			canvas.width = 480;
			canvas.height = 270;
			const context = canvas.getContext('2d');
			if (!context) continue;
			drawCameraFrame(context, spec, 0);
			const stream = canvas.captureStream(15);
			activeStreams.push(stream);
			canvases.push({ context, spec });

			const liveState: StreamState = { status: 'live', stream, message: null };
			for (const quality of ['auto', 'low', 'high'] as const) {
				streams.set(sourceKey({ source_id: spec.sourceId, quality }), liveState);
			}
		}

		const snapshot: SessionSnapshot = {
			status: 'ready',
			message: null,
			cameras: discoveredCatalog,
			streams
		};
		cardView.snapshot = snapshot;

		let frameIndex = 1;
		const timer = window.setInterval(() => {
			for (const entry of canvases) {
				drawCameraFrame(entry.context, entry.spec, frameIndex);
			}
			frameIndex += 1;
		}, 67);

		return () => {
			window.clearInterval(timer);
			for (const stream of activeStreams) {
				for (const track of stream.getTracks()) {
					track.stop();
				}
			}
			cardView.snapshot = null;
		};
	});

	$effect(() => {
		const host = editorHost;
		if (!host) return;
		const root = attachCardShadowRoot(host);
		const instance = mount(Editor, {
			target: root,
			props: {
				get config() {
					return editorConfig;
				},
				get cameras() {
					return discoveredCameras;
				},
				get message() {
					return editorError;
				},
				onchange: handleConfigChange,
				ondiscover: handleDiscoverSources
			}
		});
		return () => {
			void unmount(instance);
		};
	});

	$effect(() => {
		const host = cardHost;
		if (!host) return;
		const root = attachCardShadowRoot(host);
		const instance = mount(Card, {
			target: root,
			props: {
				state: cardView,
				onretry: () => {},
				ondemand: handleDemand
			}
		});
		return () => {
			void unmount(instance);
		};
	});
</script>

<main
	data-paper-scenario="settings.desktop.integrations"
	class="flex h-[800px] w-[1280px] flex-col overflow-hidden bg-[#181c1e] text-[#edf1f3] [font-synthesis:none]"
>
	<header
		class="flex h-14 shrink-0 items-center justify-between border-b border-[#343e44] bg-[#202629] px-6"
	>
		<div class="flex items-center gap-3">
			<span
				class="grid size-8 place-items-center rounded-md bg-[#03a9f4]/15 text-[#38bdf8]"
				aria-hidden="true"
			>
				<HomeIcon class="size-4" />
			</span>
			<div>
				<h1 class="text-sm font-semibold text-[#edf1f3]">Home Assistant · Security Dashboard</h1>
				<p class="font-mono text-2xs tracking-caps text-[#9aa8b0]">
					CUSTOM:KEEPPEEK-CARD · DIRECT BROWSER WEBRTC
				</p>
			</div>
		</div>
		<div class="flex items-center gap-2.5">
			{#if sourcesLoaded}
				<span
					data-demo-sources-loaded="true"
					class="inline-flex items-center gap-1.5 rounded-sm border border-[#79d5a4]/40 bg-[#79d5a4]/10 px-2.5 py-1 font-mono text-2xs text-[#79d5a4]"
				>
					<CheckIcon class="size-3" />
					{discoveredCameras.length} SOURCES LOADED
				</span>
			{/if}
			{#if returnedToGridAfterFocus}
				<span
					data-demo-ha-complete="true"
					class="inline-flex items-center gap-1.5 rounded-sm border border-[#38bdf8]/40 bg-[#38bdf8]/10 px-2.5 py-1 font-mono text-2xs text-[#38bdf8]"
				>
					<RadioIcon class="size-3" /> GRID RESTORED
				</span>
			{/if}
		</div>
	</header>

	<div class="grid min-h-0 flex-1 grid-cols-[448px_minmax(0,1fr)]">
		<section
			aria-label="Home Assistant card editor"
			class="flex min-h-0 flex-col border-r border-[#343e44] bg-[#1e2326]"
		>
			<div
				class="flex h-11 shrink-0 items-center justify-between border-b border-[#343e44] px-4 text-xs"
			>
				<span class="inline-flex items-center gap-2 font-medium text-[#edf1f3]">
					<SlidersHorizontalIcon class="size-3.5 text-[#38bdf8]" /> Visual editor
				</span>
				<span class="font-mono text-2xs text-[#9aa8b0]">NO PROXY · MEMORY-ONLY KEY</span>
			</div>
			<div class="ha-shadow-host min-h-0 flex-1 overflow-y-auto px-3 py-2">
				<div bind:this={editorHost}></div>
			</div>
		</section>

		<section aria-label="Home Assistant dashboard preview" class="flex min-h-0 flex-col p-6">
			<div class="mb-3 flex items-center justify-between">
				<div>
					<p class="font-mono text-2xs tracking-caps text-[#9aa8b0]">LOVELACE LIVE PREVIEW</p>
					<h2 class="mt-0.5 text-base font-semibold text-[#edf1f3]">Entrances</h2>
				</div>
				<span class="font-mono text-2xs text-[#9aa8b0]">
					LAYOUT: {String(cardView.config?.layout ?? 'grid').toUpperCase()} · COLUMNS: {cardView
						.config?.columns ?? 2}
				</span>
			</div>

			<div class="ha-shadow-host min-h-0 flex-1 overflow-y-auto">
				<div bind:this={cardHost}></div>
			</div>

			<footer
				class="mt-4 flex shrink-0 items-center justify-between border-t border-[#343e44] pt-3 font-mono text-2xs text-[#9aa8b0]"
			>
				<span>SHARED CONNECTION · 1 SESSION PER ENDPOINT + KEY</span>
				<span>NO HOME ASSISTANT MEDIA RELAY</span>
			</footer>
		</section>
	</div>
</main>

<style>
	.ha-shadow-host {
		--primary-text-color: #edf1f3;
		--secondary-text-color: #b4bec3;
		--ha-card-background: #242a2d;
		--card-background-color: #242a2d;
		--secondary-background-color: #30383c;
		--divider-color: #49555c;
		--primary-color: #38bdf8;
		--success-color: #79d5a4;
		--error-color: #ef4444;
		--ha-card-border-radius: 8px;
	}
</style>
