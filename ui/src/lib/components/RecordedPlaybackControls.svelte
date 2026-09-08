<script lang="ts">
	import { onMount } from 'svelte';
	import { Button } from './ui/button/index.js';
	import PlayIcon from '@lucide/svelte/icons/play';
	import PauseIcon from '@lucide/svelte/icons/pause';
	import VolumeIcon from '@lucide/svelte/icons/volume-2';
	import VolumeOffIcon from '@lucide/svelte/icons/volume-x';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import RotateCwIcon from '@lucide/svelte/icons/rotate-cw';
	import MaximizeIcon from '@lucide/svelte/icons/maximize';
	import MinimizeIcon from '@lucide/svelte/icons/minimize';

	type Props = {
		playing: boolean;
		muted: boolean;
		volume: number;
		rate: number;
		positionSeconds: number;
		durationSeconds: number;
		disabled: boolean;
		fullscreenTarget: HTMLElement | null;
		ontoggleplay: () => void;
		ontogglemute: () => void;
		onseek: (seconds: number) => void;
		onskip: (seconds: number) => void;
		onvolumechange: (volume: number) => void;
		onratechange: (rate: number) => void;
	};

	let {
		playing,
		muted,
		volume,
		rate,
		positionSeconds,
		durationSeconds,
		disabled,
		fullscreenTarget,
		ontoggleplay,
		ontogglemute,
		onseek,
		onskip,
		onvolumechange,
		onratechange
	}: Props = $props();
	const rates = [0.25, 0.5, 1, 1.5, 2, 4, 8];
	let fullscreen = $state(false);
	let fullscreenAvailable = $state(false);
	let fullscreenError = $state('');
	let active = false;
	let duration = $derived(Number.isFinite(durationSeconds) ? Math.max(0, durationSeconds) : 0);

	onMount(() => {
		active = true;
		fullscreenAvailable = document.fullscreenEnabled;
		return () => {
			active = false;
		};
	});

	function formatTime(seconds: number): string {
		const total = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
		const hours = Math.floor(total / 3600);
		const minutes = Math.floor((total % 3600) / 60);
		const remainder = String(total % 60).padStart(2, '0');
		return hours > 0
			? `${hours}:${String(minutes).padStart(2, '0')}:${remainder}`
			: `${minutes}:${remainder}`;
	}

	async function toggleFullscreen() {
		const target = fullscreenTarget;
		if (!target) return;
		fullscreenError = '';
		try {
			if (document.fullscreenElement === target) await document.exitFullscreen();
			else await target.requestFullscreen();
		} catch {
			if (active && target === fullscreenTarget)
				fullscreenError = 'Recording fullscreen is unavailable.';
		}
	}
</script>

<svelte:document
	onfullscreenchange={() => {
		fullscreen = fullscreenTarget !== null && document.fullscreenElement === fullscreenTarget;
	}}
/>

<div
	data-recorded-playback-controls
	role="group"
	aria-label="Playback controls"
	class="shrink-0 space-y-1 border-b pb-2"
>
	<div class="flex flex-wrap items-center gap-1">
		<Button
			variant="outline"
			size="icon"
			class="size-11"
			{disabled}
			aria-label={playing ? 'Pause recording' : 'Play recording'}
			title={playing ? 'Pause recording' : 'Play recording'}
			onclick={ontoggleplay}
		>
			{#if playing}<PauseIcon />{:else}<PlayIcon />{/if}
		</Button>
		<Button
			variant="outline"
			size="icon"
			class="size-11"
			{disabled}
			aria-label="Back 10 seconds"
			title="Back 10 seconds"
			onclick={() => onskip(-10)}
		>
			<RotateCcwIcon />
		</Button>
		<Button
			variant="outline"
			size="icon"
			class="size-11"
			{disabled}
			aria-label="Forward 10 seconds"
			title="Forward 10 seconds"
			onclick={() => onskip(10)}
		>
			<RotateCwIcon />
		</Button>
		<Button
			variant="outline"
			size="icon"
			class="size-11"
			{disabled}
			aria-label={muted ? 'Unmute recording' : 'Mute recording'}
			title={muted ? 'Unmute recording' : 'Mute recording'}
			onclick={ontogglemute}
		>
			{#if muted || volume === 0}<VolumeOffIcon />{:else}<VolumeIcon />{/if}
		</Button>
		<input
			type="range"
			aria-label="Recording volume"
			title="Recording volume"
			min="0"
			max="1"
			step="0.05"
			value={volume}
			{disabled}
			class="h-11 w-16 min-w-0 accent-primary focus-visible:outline-2 focus-visible:outline-ring"
			oninput={(event) => onvolumechange(Number(event.currentTarget.value))}
		/>
		<select
			aria-label="Playback speed"
			title="Playback speed"
			value={rate}
			{disabled}
			class="h-11 w-16 rounded-sm border border-input bg-background px-1 text-xs focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			onchange={(event) => onratechange(Number(event.currentTarget.value))}
		>
			{#if !rates.includes(rate)}<option value={rate}>{rate}x</option>{/if}
			{#each rates as speed (speed)}<option value={speed}>{speed}x</option>{/each}
		</select>
		<Button
			variant="outline"
			size="icon"
			class="ml-auto size-11"
			disabled={disabled || !fullscreenTarget || !fullscreenAvailable}
			aria-label={fullscreen ? 'Exit recording fullscreen' : 'Enter recording fullscreen'}
			title={fullscreen ? 'Exit recording fullscreen' : 'Enter recording fullscreen'}
			onclick={toggleFullscreen}
		>
			{#if fullscreen}<MinimizeIcon />{:else}<MaximizeIcon />{/if}
		</Button>
	</div>
	<div class="flex items-center gap-3">
		<span
			role="timer"
			aria-label="Playback time"
			aria-live="off"
			class="w-28 shrink-0 truncate font-mono text-xs tabular-nums"
		>
			{formatTime(positionSeconds)} / {formatTime(duration)}
		</span>
		<input
			type="range"
			aria-label="Recording position"
			min="0"
			max={duration}
			step="1"
			value={Number.isFinite(positionSeconds)
				? Math.min(duration, Math.max(0, positionSeconds))
				: 0}
			disabled={disabled || duration <= 0}
			class="h-11 min-w-0 flex-1 accent-primary focus-visible:outline-2 focus-visible:outline-ring"
			onchange={(event) => onseek(Number(event.currentTarget.value))}
		/>
	</div>
	{#if fullscreenError}<p role="alert" class="text-xs text-destructive">{fullscreenError}</p>{/if}
</div>
