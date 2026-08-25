<script lang="ts">
	import type { CameraWizardDraft } from '$lib/camera-wizard';
	import CameraCatalogEvidence from '$lib/components/CameraCatalogEvidence.svelte';
	import DesktopCameraWizardStreamsStep from '$lib/components/DesktopCameraWizardStreamsStep.svelte';
	import type { CameraCatalogCamera, CameraCatalogInfo } from '$lib/types';
	import ArrowRightIcon from '@lucide/svelte/icons/arrow-right';
	import CheckIcon from '@lucide/svelte/icons/check';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import SearchIcon from '@lucide/svelte/icons/search';

	type DemoPhase = 'discover' | 'discovered' | 'manual' | 'streams' | 'review';

	const catalogInfo: CameraCatalogInfo = {
		version: '2.1.0',
		tag: 'v2.1.0',
		generated_at: '2026-08-22T06:13:00Z',
		camera_count: 3433,
		website_url: 'https://www.cctv-database.com/'
	};

	const catalogCamera: CameraCatalogCamera = {
		id: 'reolink-rlc-811a',
		brand: 'Reolink',
		model: 'RLC-811A',
		aliases: ['RLC 811 A'],
		camera_type: 'bullet',
		resolution_label: '4K UHD',
		megapixels: 8,
		sensor: '1/2.8 inch CMOS',
		field_of_view: '105-31 horizontal',
		night_vision: 'hybrid',
		ip_rating: 'IP67',
		ik_rating: null,
		two_way_audio: true,
		release_year: 2021,
		community_notes_count: 0,
		protocols: ['onvif', 'rtsp'],
		codecs: ['H.265', 'H.264'],
		streams: [
			{ name: 'main', resolution: '3840x2160', fps: 25, codec: 'H.265' },
			{ name: 'sub', resolution: '640x360', fps: 10, codec: 'H.264' }
		],
		sources: ['https://www.cctv-database.com/'],
		stream_hints: {
			main_rtsp_url: 'rtsp://192.0.2.88:554/Preview_01_main',
			sub_rtsp_url: 'rtsp://192.0.2.88:554/Preview_01_sub'
		}
	};

	let phase = $state<DemoPhase>('discover');
	let sourceOpened = $state(false);
	let manualResultVisible = $state(false);
	let draft = $state<CameraWizardDraft>({
		ip: '192.0.2.88',
		displayName: 'Front Gate',
		username: 'operator',
		password: '',
		defaultUsernameConfigured: false,
		defaultPasswordConfigured: false,
		onvifPort: '8000',
		httpPort: '80',
		mainRtspUrl: '',
		subRtspUrl: '',
		backend: 'reo-proto',
		transport: 'tcp',
		recordGenericMotionEvents: false,
		recordingMode: 'event-boost',
		eventRecordingDurationSeconds: '60',
		discoveryEvidence: 'Manual camera address supplied'
	});

	let stageLabel = $derived(
		phase === 'discover'
			? 'Discover a camera'
			: phase === 'discovered'
				? 'Catalog match'
				: phase === 'manual'
					? 'Find a model'
					: phase === 'streams'
						? 'Assign streams'
						: 'Review setup'
	);
	let catalogStreamsApplied = $derived(
		draft.mainRtspUrl === catalogCamera.stream_hints?.main_rtsp_url &&
			draft.subRtspUrl === catalogCamera.stream_hints?.sub_rtsp_url
	);

	function applyCatalogStreams(): void {
		draft = {
			...draft,
			mainRtspUrl: catalogCamera.stream_hints?.main_rtsp_url ?? '',
			subRtspUrl: catalogCamera.stream_hints?.sub_rtsp_url ?? ''
		};
	}

	function updateDraft(update: Partial<CameraWizardDraft>): void {
		draft = { ...draft, ...update };
	}

	function markSourceOpened(): void {
		sourceOpened = true;
	}

	function selectCatalogModel(): void {
		applyCatalogStreams();
		phase = 'streams';
	}
</script>

<main
	data-paper-scenario="cameras.desktop.add-wizard"
	data-demo-catalog-wizard
	data-demo-catalog-reviewed={phase === 'review' ? 'true' : undefined}
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<aside class="flex w-[286px] shrink-0 flex-col border-r border-hairline bg-surface px-6 py-7">
		<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-primary-soft">
			<DatabaseIcon class="size-3.5" /> Embedded camera catalog
		</div>
		<h1 class="mt-3 text-[30px] leading-9 font-semibold">Set up with context.</h1>
		<p class="mt-3 text-sm leading-6 text-text-muted">
			Discovery, catalog facts, and manual research stay distinct from the first configuration
			write.
		</p>

		<div class="mt-8 space-y-4 border-y border-hairline py-5">
			{#each [['1', 'Discover', 'Device answers on your network'], ['2', 'Match', 'Known model facts and declarations'], ['3', 'Review', 'Save only when the draft is ready']] as item (item[0])}
				<div class="flex items-start gap-3">
					<span
						class="grid size-6 shrink-0 place-items-center rounded-full border font-mono text-2xs {item[1] ===
						stageLabel
							? 'border-primary bg-primary text-on-primary'
							: 'border-hairline-strong text-text-faint'}">{item[0]}</span
					>
					<div>
						<p class="text-sm font-medium">{item[1]}</p>
						<p class="mt-0.5 text-xs leading-4 text-text-faint">{item[2]}</p>
					</div>
				</div>
			{/each}
		</div>

		<div class="mt-auto rounded-sm border border-primary/30 bg-primary/5 p-3">
			<p class="font-mono text-2xs tracking-caps text-primary-soft">v{catalogInfo.version}</p>
			<p class="mt-1 text-sm font-medium">{catalogInfo.camera_count.toLocaleString()} models</p>
			<p class="mt-1 text-xs leading-5 text-text-muted">
				Compressed in the app. Reference data only.
			</p>
		</div>
	</aside>

	<section class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header
			class="flex h-[98px] shrink-0 items-end justify-between border-b border-hairline px-8 pt-6 pb-5"
		>
			<div>
				<p class="font-mono text-2xs tracking-caps text-primary-soft">CAMERA WIZARD</p>
				<h2 class="mt-1 text-xl font-semibold">{stageLabel}</h2>
			</div>
			<p class="font-mono text-2xs tracking-caps text-text-faint">NOTHING SAVED YET</p>
		</header>

		<div class="min-h-0 flex-1 overflow-hidden px-8 py-6">
			{#if phase === 'discover'}
				<div class="mx-auto flex max-w-3xl flex-col gap-6 pt-6">
					<div>
						<h3 class="text-2xl leading-8 font-semibold">One answer, already useful.</h3>
						<p class="mt-2 max-w-2xl text-sm leading-6 text-text-muted">
							Network discovery found a Reolink camera and read its model. KeepPeek can now compare
							that evidence with the embedded catalog.
						</p>
					</div>
					<button
						type="button"
						data-demo-action="use-discovery-match"
						class="flex w-full items-center gap-4 rounded-sm border border-healthy/40 bg-healthy/5 p-5 text-left"
						onclick={() => (phase = 'discovered')}
					>
						<span class="size-2 shrink-0 rounded-full bg-healthy"></span>
						<span class="min-w-0 flex-1">
							<span class="block text-base font-semibold">Front Gate</span>
							<span class="mt-1 block font-mono text-2xs text-text-faint"
								>192.0.2.77 · Reolink · RLC-811A · ONVIF</span
							>
						</span>
						<span class="inline-flex items-center gap-2 text-sm font-semibold text-primary-soft"
							>Use catalog match <ArrowRightIcon class="size-4" /></span
						>
					</button>
					<p class="font-mono text-2xs tracking-caps text-text-faint">
						DISCOVERY EVIDENCE · NO CONFIGURATION WRITE
					</p>
				</div>
			{:else if phase === 'discovered'}
				<div class="mx-auto grid max-w-4xl gap-6 pt-3 lg:grid-cols-[minmax(0,1fr)_320px]">
					<div class="space-y-5">
						<div>
							<h3 class="text-2xl leading-8 font-semibold">Known model facts, kept honest.</h3>
							<p class="mt-2 text-sm leading-6 text-text-muted">
								The match fills reference information and declared streams. It never claims the
								camera is authenticated or decoded before save.
							</p>
						</div>
						<div class="rounded-sm border border-hairline bg-raised p-4">
							<p class="font-mono text-2xs tracking-caps text-text-faint">DISCOVERED DEVICE</p>
							<p class="mt-2 text-base font-semibold">Front Gate · 192.0.2.77</p>
							<p class="mt-1 text-xs text-text-muted">Reolink identity from ONVIF discovery.</p>
						</div>
						<div class="flex flex-wrap gap-3">
							<a
								href="https://www.cctv-database.com/"
								target="_blank"
								rel="noreferrer"
								data-demo-action="open-catalog-source"
								class="inline-flex h-9 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium text-primary-soft focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								onclick={markSourceOpened}
							>
								Open CCTV Database <ExternalLinkIcon class="size-3.5" />
							</a>
							<button
								type="button"
								data-demo-action="manual-search"
								class="inline-flex h-9 items-center gap-2 rounded-sm bg-primary px-4 text-xs font-semibold text-on-primary"
								onclick={() => (phase = 'manual')}
							>
								Search a quiet camera <SearchIcon class="size-3.5" />
							</button>
						</div>
						{#if sourceOpened}
							<p data-demo-catalog-source-opened class="text-xs leading-5 text-healthy">
								Manual research opens in a separate tab. This draft remains in KeepPeek.
							</p>
						{/if}
					</div>
					<CameraCatalogEvidence camera={catalogCamera} {catalogInfo} />
				</div>
			{:else if phase === 'manual'}
				<div class="mx-auto max-w-3xl space-y-5 pt-3">
					<div>
						<h3 class="text-2xl leading-8 font-semibold">Quiet camera? Search directly.</h3>
						<p class="mt-2 text-sm leading-6 text-text-muted">
							An address and a model are enough to find relevant catalog facts before the camera is
							configured.
						</p>
					</div>
					<div class="grid gap-4 rounded-sm border border-hairline bg-raised p-5 sm:grid-cols-2">
						<label class="grid gap-2 text-xs font-medium"
							>Camera address
							<input
								class="h-10 rounded-sm border border-hairline-strong bg-surface px-3 font-mono text-xs outline-none"
								value="192.0.2.88"
								readonly
							/>
						</label>
						<label class="grid gap-2 text-xs font-medium"
							>Camera model
							<input
								class="h-10 rounded-sm border border-hairline-strong bg-surface px-3 text-xs outline-none"
								value="RLC-811A"
								readonly
							/>
						</label>
					</div>
					{#if manualResultVisible}
						<button
							type="button"
							data-demo-action="select-catalog-model"
							class="flex w-full items-center justify-between gap-4 rounded-sm border border-primary/50 bg-primary/5 px-5 py-4 text-left"
							onclick={selectCatalogModel}
						>
							<span>
								<span class="block text-base font-semibold">Reolink RLC-811A</span>
								<span class="mt-1 block font-mono text-2xs text-text-faint"
									>BULLET · 4K UHD · 8 MP · RTSP</span
								>
							</span>
							<span class="inline-flex items-center gap-2 text-sm font-semibold text-primary-soft"
								>Select model <ArrowRightIcon class="size-4" /></span
							>
						</button>
					{:else}
						<button
							type="button"
							data-demo-action="run-model-search"
							class="inline-flex h-10 items-center gap-2 rounded-sm bg-primary px-4 text-sm font-semibold text-on-primary"
							onclick={() => (manualResultVisible = true)}
						>
							<SearchIcon class="size-4" /> Search catalog
						</button>
					{/if}
				</div>
			{:else if phase === 'streams'}
				<div class="space-y-4">
					<DesktopCameraWizardStreamsStep
						{draft}
						streamHints={catalogCamera.stream_hints}
						{catalogStreamsApplied}
						streamResolution="catalog"
						onapplycatalogstreams={applyCatalogStreams}
						onupdate={updateDraft}
					/>
					<div class="flex justify-end border-t border-hairline pt-4">
						<button
							type="button"
							data-demo-action="review-setup"
							class="inline-flex h-9 items-center gap-2 rounded-sm bg-primary px-4 text-xs font-semibold text-on-primary"
							onclick={() => (phase = 'review')}
						>
							Review setup <ArrowRightIcon class="size-3.5" />
						</button>
					</div>
				</div>
			{:else}
				<div class="mx-auto grid max-w-4xl gap-6 pt-3 lg:grid-cols-[minmax(0,1fr)_320px]">
					<div class="space-y-5">
						<div>
							<div class="flex items-center gap-2 text-healthy">
								<CheckIcon class="size-5" />
								<p class="font-mono text-2xs tracking-caps">REVIEW READY</p>
							</div>
							<h3 class="mt-2 text-2xl leading-8 font-semibold">Every value stays reviewable.</h3>
							<p class="mt-2 text-sm leading-6 text-text-muted">
								The catalog filled context. The operator still controls the editable stream
								endpoints and the moment configuration is written.
							</p>
						</div>
						<dl
							class="divide-y divide-hairline rounded-sm border border-hairline bg-raised text-sm"
						>
							{#each [['Address', draft.ip], ['Model', `${catalogCamera.brand} ${catalogCamera.model}`], ['Recording', draft.mainRtspUrl || 'No explicit stream'], ['Live', draft.subRtspUrl || 'No explicit stream'], ['Configuration write', 'Not sent yet']] as row (row[0])}
								<div class="flex items-center justify-between gap-5 px-4 py-3">
									<dt class="text-text-muted">{row[0]}</dt>
									<dd class="max-w-[66%] truncate text-right font-mono text-xs">{row[1]}</dd>
								</div>
							{/each}
						</dl>
						<p
							class="rounded-sm border border-primary/30 bg-primary/5 px-4 py-3 text-sm leading-6 text-text-muted"
						>
							Saving is the first configuration write. Catalog facts remain reference material, not
							runtime proof.
						</p>
					</div>
					<CameraCatalogEvidence camera={catalogCamera} {catalogInfo} compact />
				</div>
			{/if}
		</div>

		<footer
			class="flex h-[52px] shrink-0 items-center justify-between border-t border-hairline px-8"
		>
			<span class="font-mono text-2xs tracking-caps text-text-faint"
				>CATALOG REFERENCE · OPERATOR REVIEW</span
			>
			<span class="font-mono text-2xs text-primary-soft">KEEPPEEK</span>
		</footer>
	</section>
</main>
