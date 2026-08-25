<script lang="ts">
	import { resolve } from '$app/paths';
	import type { CameraFleetPresentation } from '$lib/camera-fleet';
	import type { CameraListItem } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';

	type Props = {
		camera: CameraListItem;
		presentation: CameraFleetPresentation;
		selected: boolean;
		tabindex: number;
		paperFrame?: boolean;
		onselect: (selected: boolean) => void;
		onfocus: () => void;
		onkeydown: (event: KeyboardEvent) => void;
	};

	let {
		camera,
		presentation,
		selected,
		tabindex,
		paperFrame = false,
		onselect,
		onfocus,
		onkeydown
	}: Props = $props();

	let label = $derived(camera.name ?? camera.id);
	let detail = $derived([camera.ip, camera.model].filter(Boolean).join(' · '));
	let cameraHref = $derived(`${resolve('/camera')}?camera=${encodeURIComponent(camera.id)}`);

	function stateColor(): string {
		if (presentation.state === 'healthy') return 'bg-healthy';
		if (
			presentation.state === 'degraded' ||
			presentation.state === 'stale' ||
			presentation.state === 'reconnecting'
		)
			return 'bg-activity';
		if (presentation.state === 'offline') return 'bg-live';
		return 'bg-text-faint';
	}

	function stateTextColor(): string {
		if (
			presentation.state === 'degraded' ||
			presentation.state === 'stale' ||
			presentation.state === 'reconnecting'
		)
			return 'text-activity';
		if (presentation.state === 'offline') return 'text-live-text';
		return 'text-text-faint';
	}

	function recordingColor(): string {
		if (presentation.recordingState === 'healthy') return 'bg-healthy';
		if (presentation.recordingState === 'degraded') return 'bg-activity';
		return 'bg-text-faint';
	}
</script>

<div
	data-fleet-row={camera.id}
	data-fleet-row-height="56"
	class="h-14 border-b border-hairline text-xs"
>
	<div class="grid h-full grid-cols-[44px_12px_minmax(0,1fr)_44px] items-center md:hidden">
		<label class="relative grid size-11 cursor-pointer place-items-center">
			<input
				type="checkbox"
				class="peer absolute inset-0 size-11 cursor-pointer opacity-0"
				aria-label={`Select ${label}`}
				checked={selected}
				onchange={(event) => onselect(event.currentTarget.checked)}
			/>
			<span
				class="pointer-events-none grid size-[13px] place-items-center rounded-xs border border-hairline-strong bg-raised peer-focus-visible:ring-2 peer-focus-visible:ring-ring"
			>
				{#if selected}<CheckIcon class="size-3 text-primary" strokeWidth={3} />{/if}
			</span>
		</label>
		<span class="size-[7px] rounded-full {stateColor()}"></span>
		<div class="min-w-0 px-2">
			<a
				data-fleet-focus={camera.id}
				href={cameraHref}
				class="block truncate text-sm font-medium focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				{tabindex}
				{onfocus}
				{onkeydown}
			>
				{label}
			</a>
			<p class="truncate font-mono text-2xs {stateTextColor()}">
				{presentation.state === 'healthy' ? detail : presentation.statusDetail}
			</p>
		</div>
		<a
			href={cameraHref}
			class="grid size-11 place-items-center text-text-faint focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			aria-label={`Open ${label}`}
		>
			<ChevronRightIcon class="size-4" />
		</a>
	</div>
	<div
		class="hidden h-full items-center md:grid {paperFrame
			? 'grid-cols-[32px_20px_270px_140px_230px_150px_140px_120px_152px_80px]'
			: 'grid-cols-[32px_20px_270px_140px_230px_150px_140px_120px_152px_60px]'}"
	>
		<div>
			<input
				type="checkbox"
				class="size-[13px] accent-primary"
				aria-label={`Select ${label}`}
				checked={selected}
				onchange={(event) => onselect(event.currentTarget.checked)}
			/>
		</div>
		<span class="size-[7px] rounded-full {stateColor()}"></span>
		<div class="min-w-0 pr-4">
			<a
				data-fleet-focus={camera.id}
				href={cameraHref}
				class="block truncate text-sm font-medium hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				{tabindex}
				{onfocus}
				{onkeydown}
			>
				{label}
			</a>
			<p
				class="truncate font-mono text-2xs {presentation.state === 'healthy'
					? 'text-text-faint'
					: stateTextColor()}"
			>
				{presentation.state === 'healthy' ? detail : presentation.statusDetail}
			</p>
		</div>
		<div class="min-w-0 pr-4">
			<p class="truncate text-[13px]">{presentation.transport}</p>
			<p class="truncate font-mono text-2xs text-text-faint">
				{presentation.transportDetail}
			</p>
		</div>
		<div class="flex min-w-0 gap-1.5 overflow-hidden pr-4">
			{#each presentation.streams as stream (stream)}
				<span
					class="shrink-0 rounded-xs border border-hairline bg-raised px-1.5 py-0.5 font-mono text-[10px] text-text-muted"
				>
					{stream}
				</span>
			{:else}
				<span
					class="rounded-xs border border-dashed border-hairline-strong px-1.5 py-0.5 font-mono text-[10px] text-text-faint"
				>
					NO STREAMS REPORTED
				</span>
			{/each}
		</div>
		<div class="flex items-center gap-2 pr-4">
			<span class="size-1.5 rounded-full {recordingColor()}"></span>
			<span>{presentation.recording}</span>
		</div>
		<span class="font-mono text-[13px] {presentation.throughput ? '' : 'text-text-faint'}">
			{presentation.throughput ?? '—'}
		</span>
		<span class="font-mono text-[13px] {presentation.gbPerDay ? '' : 'text-text-faint'}">
			{presentation.gbPerDay ?? '—'}
		</span>
		<div>
			<p class="font-mono text-2xs text-text-faint">—</p>
			<p class="text-2xs text-text-faint">Not reported</p>
		</div>
		<div class="flex justify-end pr-2">
			{#if presentation.state !== 'healthy' && presentation.state !== 'starting'}
				<a
					href={resolve('/system-health')}
					class="inline-flex h-6 items-center rounded-sm border border-hairline-strong px-2 text-2xs font-medium focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				>
					Diagnose
				</a>
			{:else}
				<a
					href={cameraHref}
					class="grid size-7 place-items-center rounded-sm text-text-faint hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					aria-label={`Open ${label}`}
				>
					<ChevronRightIcon class="size-3.5" />
				</a>
			{/if}
		</div>
	</div>
</div>
