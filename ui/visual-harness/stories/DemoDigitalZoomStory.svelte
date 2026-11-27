<script lang="ts">
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import FocusedMediaViewport from '$lib/components/FocusedMediaViewport.svelte';
	import ShieldCheckIcon from '@lucide/svelte/icons/shield-check';

	let viewportHost = $state<HTMLDivElement | null>(null);
	let scale = $state(1);
	let peakScale = $state(1);
	let panX = $state(0);
	let panY = $state(0);
	let hasPanned = $state(false);
	let panUpdates = $state(0);

	let completedCycle = $derived(peakScale >= 4 && hasPanned && scale === 1);

	function handleZoomChange(nextScale: number): void {
		scale = nextScale;
		if (nextScale > peakScale) peakScale = nextScale;
	}

	function parseTransformPan(transform: string): { x: number; y: number } {
		const match = /translate\(\s*([-\d.]+)px,\s*([-\d.]+)px\)/.exec(transform);
		if (!match) return { x: 0, y: 0 };
		return { x: Number(match[1]) || 0, y: Number(match[2]) || 0 };
	}

	$effect(() => {
		const host = viewportHost;
		if (!host) return;
		const layer = host.querySelector<HTMLElement>('[data-focused-media-layer]');
		if (!layer) return;

		const syncTransform = () => {
			const next = parseTransformPan(layer.style.transform);
			if (next.x !== panX || next.y !== panY) {
				panX = next.x;
				panY = next.y;
				if (Math.abs(next.x) > 1 || Math.abs(next.y) > 1) {
					hasPanned = true;
					panUpdates += 1;
				}
			}
		};

		syncTransform();
		const observer = new MutationObserver(syncTransform);
		observer.observe(layer, { attributes: true, attributeFilter: ['style'] });
		return () => observer.disconnect();
	});
</script>

<main
	data-paper-scenario="peek.desktop.live-wall"
	data-demo-digital-zoom
	data-demo-digital-zoom-complete={completedCycle ? 'true' : undefined}
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="viewer" paperFull />

	<section class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header
			class="flex h-14 shrink-0 items-center justify-between gap-4 border-b border-hairline bg-surface px-5"
		>
			<div class="flex min-w-0 items-center gap-3">
				<span class="size-2 shrink-0 rounded-full bg-healthy"></span>
				<h1 class="truncate text-sm font-semibold">Front Driveway</h1>
				<span class="font-mono text-2xs text-text-faint">192.0.2.41 · VIEWER</span>
			</div>
			<div class="flex items-center gap-2 font-mono text-2xs">
				<span
					class="inline-flex items-center gap-1.5 rounded-sm border border-healthy/40 bg-healthy/10 px-2.5 py-1 text-healthy"
				>
					<ShieldCheckIcon class="size-3.5" aria-hidden="true" />
					PHYSICAL PTZ: UNTOUCHED
				</span>
				<span class="rounded-sm border border-hairline bg-raised px-2.5 py-1 text-text-muted">
					DECODED 3840×2160 · H.264
				</span>
				<span
					class="rounded-sm border border-primary/40 bg-primary/10 px-2.5 py-1 text-primary-soft tabular-nums"
				>
					DIGITAL {scale.toFixed(1)}x
				</span>
			</div>
		</header>

		<div
			bind:this={viewportHost}
			class="min-h-0 flex-1 overflow-hidden bg-video"
			style="--focused-media-toolbar-height: 3rem; --focused-media-control-size: 2rem; --focused-media-control-padding: 0.1875rem;"
		>
			<FocusedMediaViewport
				mediaKey="front-driveway-live"
				aspectRatio={16 / 9}
				controlsPosition="top-left"
				onzoomchange={handleZoomChange}
			>
				{#snippet toolbar()}
					<div class="flex items-center gap-2 pr-3 font-mono text-2xs">
						<span class="rounded-sm bg-live/90 px-2 py-1 font-semibold tracking-caps text-white">
							LIVE
						</span>
						<span class="rounded-sm border border-hairline bg-surface px-2.5 py-1 text-text-muted">
							History
						</span>
						<span class="rounded-sm border border-hairline bg-surface px-2.5 py-1 text-text-muted">
							Main · 4K · 25 FPS
						</span>
						<span class="text-text-faint">
							Drag to pan · Arrows 40 px · 0 / Esc resets · Browser-side only
						</span>
					</div>
				{/snippet}

				<svg
					viewBox="0 0 1600 900"
					class="block size-full select-none"
					role="img"
					aria-label="Front Driveway live frame showing porch parcel and delivery van license plate KP-4829"
				>
					<defs>
						<linearGradient id="dz-sky" x1="0" y1="0" x2="0" y2="1">
							<stop offset="0%" stop-color="#172438" />
							<stop offset="60%" stop-color="#283d58" />
							<stop offset="100%" stop-color="#3c4d63" />
						</linearGradient>
						<linearGradient id="dz-driveway" x1="0" y1="0" x2="1" y2="1">
							<stop offset="0%" stop-color="#2b313a" />
							<stop offset="100%" stop-color="#1d222a" />
						</linearGradient>
						<linearGradient id="dz-van" x1="0" y1="0" x2="0" y2="1">
							<stop offset="0%" stop-color="#e7edf5" />
							<stop offset="100%" stop-color="#b7c4d4" />
						</linearGradient>
					</defs>

					<rect width="1600" height="400" fill="url(#dz-sky)" />
					<rect y="390" width="1600" height="510" fill="#1e2a23" />
					<polygon points="480,390 1290,390 1600,900 180,900" fill="url(#dz-driveway)" />
					<line
						x1="505"
						y1="390"
						x2="230"
						y2="900"
						stroke="#4b5563"
						stroke-width="6"
						stroke-dasharray="28 18"
					/>
					<line
						x1="1265"
						y1="390"
						x2="1555"
						y2="900"
						stroke="#4b5563"
						stroke-width="6"
						stroke-dasharray="28 18"
					/>

					<rect
						x="70"
						y="140"
						width="560"
						height="320"
						rx="4"
						fill="#2c3340"
						stroke="#465164"
						stroke-width="3"
					/>
					<polygon
						points="45,148 350,48 655,148"
						fill="#1f2530"
						stroke="#465164"
						stroke-width="3"
					/>
					<rect
						x="120"
						y="210"
						width="120"
						height="96"
						rx="3"
						fill="#f5d488"
						fill-opacity="0.22"
						stroke="#64748b"
						stroke-width="3"
					/>
					<line x1="180" y1="210" x2="180" y2="306" stroke="#64748b" stroke-width="2" />
					<line x1="120" y1="258" x2="240" y2="258" stroke="#64748b" stroke-width="2" />

					<rect
						x="325"
						y="222"
						width="110"
						height="238"
						rx="3"
						fill="#3b2f2a"
						stroke="#5c4b43"
						stroke-width="3"
					/>
					<rect x="345" y="246" width="70" height="78" rx="2" fill="#93c5fd" fill-opacity="0.24" />
					<circle cx="418" cy="348" r="5" fill="#d4b483" />
					<rect x="454" y="304" width="30" height="44" rx="3" fill="#111827" stroke="#64748b" />
					<circle cx="469" cy="316" r="4" fill="#38bdf8" />
					<rect x="461" y="326" width="16" height="14" rx="2" fill="#334155" />

					<rect x="295" y="458" width="230" height="24" fill="#475569" />
					<rect
						x="456"
						y="412"
						width="72"
						height="46"
						rx="2"
						fill="#b98a55"
						stroke="#7c562c"
						stroke-width="2"
					/>
					<line x1="456" y1="428" x2="528" y2="428" stroke="#7c562c" stroke-width="2" />
					<rect x="466" y="434" width="52" height="18" rx="2" fill="#f8fafc" />
					<text
						x="492"
						y="446"
						text-anchor="middle"
						fill="#0f172a"
						font-family="monospace"
						font-size="10"
						font-weight="700"
					>
						PKG-0914
					</text>

					<rect x="650" y="250" width="900" height="140" fill="#232c38" />
					<g stroke="#334155" stroke-width="2">
						<line x1="740" y1="250" x2="740" y2="390" />
						<line x1="860" y1="250" x2="860" y2="390" />
						<line x1="980" y1="250" x2="980" y2="390" />
						<line x1="1100" y1="250" x2="1100" y2="390" />
						<line x1="1220" y1="250" x2="1220" y2="390" />
						<line x1="1340" y1="250" x2="1340" y2="390" />
						<line x1="1460" y1="250" x2="1460" y2="390" />
					</g>

					<g transform="translate(860, 390)">
						<ellipse cx="195" cy="252" rx="185" ry="26" fill="#090c12" fill-opacity="0.55" />
						<rect
							x="28"
							y="16"
							width="334"
							height="196"
							rx="24"
							fill="url(#dz-van)"
							stroke="#64748b"
							stroke-width="3"
						/>
						<rect x="52" y="38" width="286" height="76" rx="10" fill="#1e293b" />
						<rect x="64" y="48" width="126" height="56" rx="5" fill="#334155" fill-opacity="0.7" />
						<rect x="200" y="48" width="126" height="56" rx="5" fill="#334155" fill-opacity="0.7" />
						<rect x="44" y="138" width="38" height="22" rx="4" fill="#ef4444" />
						<rect x="308" y="138" width="38" height="22" rx="4" fill="#ef4444" />
						<rect x="44" y="164" width="38" height="12" rx="3" fill="#f59e0b" />
						<rect x="308" y="164" width="38" height="12" rx="3" fill="#f59e0b" />
						<rect x="18" y="188" width="354" height="32" rx="8" fill="#1f2937" />
						<rect x="50" y="206" width="52" height="44" rx="10" fill="#0f172a" />
						<rect x="288" y="206" width="52" height="44" rx="10" fill="#0f172a" />

						<rect
							x="126"
							y="148"
							width="138"
							height="38"
							rx="5"
							fill="#f8fafc"
							stroke="#0f172a"
							stroke-width="3"
						/>
						<rect x="131" y="153" width="128" height="28" rx="3" fill="none" stroke="#2563eb" />
						<text
							x="195"
							y="174"
							text-anchor="middle"
							fill="#0f172a"
							font-family="monospace"
							font-size="22"
							font-weight="800"
							letter-spacing="2"
						>
							KP-4829
						</text>
						<text
							x="195"
							y="134"
							text-anchor="middle"
							fill="#334155"
							font-family="monospace"
							font-size="11"
							font-weight="700"
							letter-spacing="2"
						>
							LOCAL COURIER · UNIT 14
						</text>
					</g>

					<rect x="24" y="24" width="340" height="58" rx="4" fill="#090c12" fill-opacity="0.72" />
					<text
						x="40"
						y="50"
						fill="#f8fafc"
						font-family="monospace"
						font-size="19"
						font-weight="600"
					>
						2026-08-18 06:37:23 UTC
					</text>
					<text x="40" y="71" fill="#94a3b8" font-family="monospace" font-size="13">
						FRONT DRIVEWAY · 3840×2160 · 25 FPS
					</text>
				</svg>
			</FocusedMediaViewport>
		</div>

		<footer
			class="flex h-12 shrink-0 items-center justify-between border-t border-hairline bg-surface px-5 font-mono text-2xs text-text-muted"
		>
			<div class="flex items-center gap-4">
				<span>MAGNIFICATION: <strong class="text-foreground">{scale.toFixed(1)}x</strong></span>
				<span>
					PAN OFFSET:
					<strong class="text-foreground tabular-nums">
						{Math.round(panX)}px, {Math.round(panY)}px
					</strong>
				</span>
				<span>PTZ COMMANDS SENT: <strong class="text-healthy">0</strong></span>
			</div>
			<div class="flex items-center gap-3">
				{#if completedCycle}
					<span class="text-healthy">1.0x RESET VERIFIED</span>
				{:else if panUpdates > 0}
					<span class="text-primary-soft">BOUNDED PAN ACTIVE</span>
				{:else}
					<span class="text-text-faint">ORIGINAL STREAM &amp; EXPORTS UNCHANGED</span>
				{/if}
			</div>
		</footer>
	</section>
</main>
