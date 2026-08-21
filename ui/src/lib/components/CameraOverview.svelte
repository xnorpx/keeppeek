<script lang="ts">
	import { presentCameraControl } from '$lib/camera-control';
	import type { CameraHealth, CameraListItem } from '$lib/types';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import CameraPtzControl from './CameraPtzControl.svelte';
	import LiveVideo from './LiveVideo.svelte';

	type Props = {
		camera: CameraListItem;
		health: CameraHealth | null;
		stream: 'main' | 'sub';
		previewAvailable: boolean;
		commandTransportAvailable?: boolean;
		paperFrame?: boolean;
	};

	let {
		camera,
		health,
		stream,
		previewAvailable,
		commandTransportAvailable = false,
		paperFrame = false
	}: Props = $props();
	let label = $derived(camera.name ?? camera.id);
	let control = $derived(presentCameraControl(camera, commandTransportAvailable));
	let previewStatus = $derived(
		health === null
			? 'Waiting for camera health'
			: health.state === 'offline'
				? (health.last_error ?? 'Camera is offline')
				: health.configured_profiles.length === 0
					? 'No configured media profile was reported'
					: 'Live preview is unavailable'
	);
	let previewProfile = $derived(
		camera.profiles.find((profile) => profile.stream === stream) ?? camera.profiles[0] ?? null
	);
	let previewHealth = $derived(
		health?.streams.find((candidate) => candidate.type.includes(stream)) ??
			health?.streams[0] ??
			null
	);
</script>

<section
	id="overview"
	data-camera-overview-paper-frame={paperFrame || undefined}
	class="scroll-mt-16 overflow-hidden {paperFrame
		? 'h-[394px] w-[1130px] bg-ground [font-synthesis:none]'
		: 'rounded-md border border-hairline bg-surface'}"
	aria-labelledby="overview-heading"
>
	{#if !paperFrame}
		<header class="flex min-h-12 items-center gap-2 border-b border-hairline px-4">
			<RadioIcon class="size-4 text-primary-soft" />
			<h2 id="overview-heading" class="text-sm font-semibold">Live preview</h2>
			<span class="ml-auto font-mono text-2xs tracking-caps text-text-faint"
				>{stream.toUpperCase()}</span
			>
		</header>
	{/if}
	<div
		class={paperFrame
			? 'flex h-[394px] w-[1130px] gap-5'
			: 'grid lg:grid-cols-[minmax(0,1fr)_18rem]'}
	>
		<div
			class="relative min-w-0 bg-video {paperFrame
				? 'flex h-[394px] w-[700px] shrink-0 flex-col justify-between rounded-lg border border-hairline p-3'
				: 'aspect-video'}"
		>
			{#if paperFrame}
				<div class="flex items-start justify-between gap-3">
					<div class="rounded-sm bg-video/80 px-2.5 py-[7px] font-mono text-white">
						<p class="text-xs leading-4">
							LAST SAMPLE · {previewHealth?.report_age_ms ?? '—'}MS AGO
						</p>
						<p class="mt-[5px] text-[10px] leading-3 tracking-[0.08em] text-white/70">
							{stream.toUpperCase()} · {previewProfile?.resolution ?? 'RESOLUTION NOT REPORTED'} · {previewProfile?.framerate ??
								'—'} FPS · {previewProfile?.encoding?.toUpperCase() ?? 'CODEC NOT REPORTED'}
						</p>
					</div>
					<span
						class="rounded-full bg-video/80 px-2.5 py-1.5 font-mono text-[10px] leading-3 tracking-[0.12em] text-white/70"
					>
						RECORDING STATUS NOT REPORTED
					</span>
				</div>
				<p class="font-mono text-[11px] leading-[14px] tracking-[0.14em] text-text-faint">
					LIVE PREVIEW · MEDIA PIXELS OMITTED FROM THE DETERMINISTIC STORY
				</p>
				<div
					class="flex h-6 items-center gap-2.5 font-mono text-[10px] leading-3 tracking-[0.08em] text-white/60"
				>
					<span class="rounded-sm bg-video/80 px-2.5 py-1.5 text-white">{stream.toUpperCase()}</span
					>
					<span class="flex-1"></span>
					<span
						>{previewHealth?.drops ?? 0} DROPPED · {previewHealth?.reconnects ?? 0} RECONNECTS</span
					>
				</div>
			{:else if previewAvailable}
				<LiveVideo cameraId={camera.id} {stream} quality="auto" class="size-full overflow-hidden" />
			{:else}
				<div class="absolute inset-0 grid place-items-center px-6 text-center">
					<div class="space-y-2">
						<RadioIcon class="mx-auto size-5 text-text-faint" />
						<p class="text-sm font-medium text-white">Preview unavailable</p>
						<p class="text-xs text-text-muted">{previewStatus}</p>
					</div>
				</div>
			{/if}
			{#if !paperFrame}
				<span
					class="pointer-events-none absolute top-3 left-3 rounded-sm bg-video/80 px-2 py-1 text-xs font-medium text-white"
					>{label}</span
				>
			{/if}
		</div>

		{#if control.showPtz}
			<CameraPtzControl
				cameraId={camera.id}
				commandAvailable={control.commandAvailable}
				reason={control.reason}
				variant={paperFrame ? 'paper' : 'desktop'}
			/>
		{/if}
	</div>
</section>
