<script lang="ts">
	import { resolve } from '$app/paths';
	import ArchiveIcon from '@lucide/svelte/icons/archive';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import ScanLineIcon from '@lucide/svelte/icons/scan-line';

	type Props = {
		cameraCount: number;
		emptyDayLabel?: string;
		paperFrame?: boolean;
	};

	let { cameraCount, emptyDayLabel, paperFrame = false }: Props = $props();
</script>

<aside
	data-first-run-empty-states
	class={paperFrame ? 'flex h-[572px] w-[708px] shrink-0 flex-col gap-5' : 'space-y-4'}
	aria-label="First-run empty states"
>
	<section
		data-empty-state="cameras"
		class="grid place-items-center rounded-md border border-hairline bg-surface text-center {paperFrame
			? 'h-[300px] w-[708px] shrink-0'
			: 'min-h-[18rem] p-5'}"
		aria-labelledby="empty-cameras-heading"
	>
		<div class="flex flex-col items-center {paperFrame ? 'gap-[14px]' : 'max-w-md gap-3'}">
			<CameraIcon
				class="text-text-faint {paperFrame ? 'size-[34px]' : 'size-8'}"
				strokeWidth={paperFrame ? 1.4 : 2}
			/>
			<div class={paperFrame ? 'flex flex-col items-center gap-[14px]' : ''}>
				<h2
					id="empty-cameras-heading"
					class="font-semibold {paperFrame ? 'text-xl leading-6' : 'text-lg'}"
				>
					{cameraCount === 0 ? 'No cameras yet' : `${cameraCount} cameras configured`}
				</h2>
				<p
					class="text-sm text-text-muted {paperFrame
						? 'mt-0 w-[420px] leading-[22px]'
						: 'mt-1 leading-6'}"
				>
					KeepPeek can look for cameras on this network, or you can paste an RTSP URL if you already
					know it.
				</p>
			</div>
			<div class="flex flex-wrap justify-center {paperFrame ? 'gap-3 pt-1' : 'gap-2'}">
				<a
					href={`${resolve('/cameras/new')}#discover-camera`}
					class="inline-flex items-center gap-2 rounded-sm bg-primary font-semibold text-on-primary {paperFrame
						? 'h-[38px] w-[155px] justify-center px-5 text-sm'
						: 'h-9 px-4 text-xs'}"
				>
					{#if !paperFrame}<ScanLineIcon class="size-3.5" />{/if} Scan this network
				</a>
				<a
					href={`${resolve('/cameras/new')}#manual-camera`}
					class="inline-flex items-center gap-2 rounded-sm border border-hairline-strong bg-raised font-medium {paperFrame
						? 'h-[38px] w-[139px] justify-center px-4 text-sm'
						: 'h-9 px-4 text-xs'}"
				>
					{#if !paperFrame}<CameraIcon class="size-3.5" />{/if} Enter an address
				</a>
			</div>
		</div>
	</section>

	<section
		data-empty-state="keep"
		class="flex flex-col rounded-md border border-hairline bg-surface sm:flex-row sm:items-center {paperFrame
			? 'h-[116px] w-[708px] shrink-0 gap-5 p-[22px]'
			: 'gap-4 p-5 sm:justify-between'}"
	>
		<div class={paperFrame ? 'flex w-[430px] shrink-0 flex-col gap-1.5' : 'max-w-sm'}>
			<h2
				class="flex items-center gap-2 font-semibold {paperFrame
					? 'text-lg leading-[22px]'
					: 'text-base'}"
			>
				{#if !paperFrame}<ArchiveIcon class="size-4" />{/if} Keep, before there is anything to keep
			</h2>
			<p class="text-text-muted {paperFrame ? 'text-[13px] leading-5' : 'mt-1 text-xs leading-5'}">
				The timeline still draws — it just shows an honest empty day rather than a spinner or a
				shrug. Recording starts the moment a camera is saved.
			</p>
		</div>
		<a
			href={resolve('/keep')}
			class="flex w-[212px] shrink-0 flex-col gap-1.5 font-mono text-2xs tracking-caps text-text-faint"
		>
			<span class="flex h-11 w-[212px] gap-0.5">
				<span class="h-11 w-[52px] rounded-[2px] bg-raised"></span>
				<span class="h-11 w-[52px] rounded-[2px] bg-raised"></span>
				<span class="h-11 w-[52px] rounded-[2px] bg-raised"></span>
				<span class="h-11 w-[52px] rounded-[2px] bg-raised"></span>
			</span>
			<span class="text-[10px] leading-3 tracking-[0.08em]">
				{emptyDayLabel ? `NO FOOTAGE ON ${emptyDayLabel}` : 'NO FOOTAGE YET'}
			</span>
		</a>
	</section>

	<section
		data-empty-state="events"
		class="flex flex-col rounded-md border border-hairline bg-surface sm:flex-row sm:items-center {paperFrame
			? 'h-[116px] w-[708px] shrink-0 gap-5 p-[22px]'
			: 'gap-4 p-5 sm:justify-between'}"
	>
		<div class={paperFrame ? 'flex w-[430px] shrink-0 flex-col gap-1.5' : 'max-w-sm'}>
			<h2
				class="flex items-center gap-2 font-semibold {paperFrame
					? 'text-lg leading-[22px]'
					: 'text-base'}"
			>
				{#if !paperFrame}<ScanLineIcon class="size-4" />{/if} Event source registry unavailable
			</h2>
			<p class="text-text-muted {paperFrame ? 'text-[13px] leading-5' : 'mt-1 text-xs leading-5'}">
				No event-source configuration API is available, so this is not presented as “no results.”
			</p>
		</div>
		<div class="flex w-[212px] shrink-0 flex-col gap-2">
			<span class="font-mono text-2xs leading-[14px] tracking-[0.1em] text-text-faint">
				EVENT SOURCE REGISTRY UNAVAILABLE
			</span>
			<button
				type="button"
				class="h-8 self-start rounded-sm border border-hairline-strong bg-raised px-3 text-xs text-text-muted"
				disabled
				title="Event-source configuration is unavailable"
			>
				Registry unavailable
			</button>
		</div>
	</section>
</aside>
