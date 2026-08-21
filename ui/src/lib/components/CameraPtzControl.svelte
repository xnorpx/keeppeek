<script lang="ts">
	import { useControlClient } from '$lib/control-context';
	import type { PtzPreset } from '$lib/control-client';
	import ArrowDownIcon from '@lucide/svelte/icons/arrow-down';
	import ArrowDownLeftIcon from '@lucide/svelte/icons/arrow-down-left';
	import ArrowDownRightIcon from '@lucide/svelte/icons/arrow-down-right';
	import ArrowLeftIcon from '@lucide/svelte/icons/arrow-left';
	import ArrowRightIcon from '@lucide/svelte/icons/arrow-right';
	import ArrowUpIcon from '@lucide/svelte/icons/arrow-up';
	import ArrowUpLeftIcon from '@lucide/svelte/icons/arrow-up-left';
	import ArrowUpRightIcon from '@lucide/svelte/icons/arrow-up-right';
	import CircleStopIcon from '@lucide/svelte/icons/circle-stop';
	import MinusIcon from '@lucide/svelte/icons/minus';
	import PlusIcon from '@lucide/svelte/icons/plus';

	type Props = {
		cameraId: string;
		commandAvailable: boolean;
		reason: string | null;
		variant?: 'desktop' | 'mobile' | 'paper';
	};

	let { cameraId, commandAvailable, reason, variant = 'desktop' }: Props = $props();
	const controlClient = useControlClient();
	let presets = $state.raw<PtzPreset[]>([]);
	let ptzError = $state<string | null>(null);
	let loadingPresets = $state(false);
	const directions = [
		{
			label: 'Move up-left',
			icon: ArrowUpLeftIcon,
			position: 'col-start-1 row-start-1',
			movement: { pan: -1, tilt: 1, zoom: 0 }
		},
		{
			label: 'Tilt up',
			icon: ArrowUpIcon,
			position: 'col-start-2 row-start-1',
			movement: { pan: 0, tilt: 1, zoom: 0 }
		},
		{
			label: 'Move up-right',
			icon: ArrowUpRightIcon,
			position: 'col-start-3 row-start-1',
			movement: { pan: 1, tilt: 1, zoom: 0 }
		},
		{
			label: 'Pan left',
			icon: ArrowLeftIcon,
			position: 'col-start-1 row-start-2',
			movement: { pan: -1, tilt: 0, zoom: 0 }
		},
		{
			label: 'Stop PTZ',
			icon: CircleStopIcon,
			position: 'col-start-2 row-start-2',
			movement: null
		},
		{
			label: 'Pan right',
			icon: ArrowRightIcon,
			position: 'col-start-3 row-start-2',
			movement: { pan: 1, tilt: 0, zoom: 0 }
		},
		{
			label: 'Move down-left',
			icon: ArrowDownLeftIcon,
			position: 'col-start-1 row-start-3',
			movement: { pan: -1, tilt: -1, zoom: 0 }
		},
		{
			label: 'Tilt down',
			icon: ArrowDownIcon,
			position: 'col-start-2 row-start-3',
			movement: { pan: 0, tilt: -1, zoom: 0 }
		},
		{
			label: 'Move down-right',
			icon: ArrowDownRightIcon,
			position: 'col-start-3 row-start-3',
			movement: { pan: 1, tilt: -1, zoom: 0 }
		}
	] as const;

	$effect(() => {
		const sourceId = cameraId;
		if (!commandAvailable) {
			presets = [];
			return;
		}
		let cancelled = false;
		loadingPresets = true;
		void controlClient
			.getPtzPresets(sourceId)
			.then((next) => {
				if (!cancelled) presets = next;
			})
			.catch((cause) => {
				if (!cancelled) ptzError = errorMessage(cause, 'Unable to load PTZ presets.');
			})
			.finally(() => {
				if (!cancelled) loadingPresets = false;
			});
		return () => {
			cancelled = true;
		};
	});

	function errorMessage(cause: unknown, fallback: string): string {
		return cause instanceof Error ? cause.message : fallback;
	}

	async function startPtz(movement: { pan: number; tilt: number; zoom: number }): Promise<void> {
		if (!commandAvailable) return;
		ptzError = null;
		try {
			await controlClient.movePtz(cameraId, movement);
		} catch (cause) {
			ptzError = errorMessage(cause, 'PTZ movement failed.');
		}
	}

	async function stopPtz(): Promise<void> {
		if (!commandAvailable) return;
		try {
			await controlClient.stopPtz(cameraId);
		} catch (cause) {
			ptzError = errorMessage(cause, 'PTZ stop failed.');
		}
	}

	function handlePointerDown(
		event: PointerEvent,
		movement: { pan: number; tilt: number; zoom: number }
	) {
		event.preventDefault();
		(event.currentTarget as HTMLButtonElement).setPointerCapture(event.pointerId);
		void startPtz(movement);
	}

	function handlePointerEnd(event: PointerEvent): void {
		const button = event.currentTarget as HTMLButtonElement;
		if (button.hasPointerCapture(event.pointerId)) button.releasePointerCapture(event.pointerId);
		void stopPtz();
	}

	function handlePtzKeydown(
		event: KeyboardEvent,
		movement: { pan: number; tilt: number; zoom: number }
	): void {
		if ((event.key === 'Enter' || event.key === ' ') && !event.repeat) {
			event.preventDefault();
			void startPtz(movement);
		}
	}

	function handlePtzKeyup(event: KeyboardEvent): void {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			void stopPtz();
		}
	}

	async function gotoPreset(preset: PtzPreset): Promise<void> {
		ptzError = null;
		try {
			await controlClient.gotoPtzPreset(cameraId, preset.id);
		} catch (cause) {
			ptzError = errorMessage(cause, 'PTZ preset failed.');
		}
	}
</script>

<div
	data-camera-ptz-control={variant}
	class={variant === 'desktop'
		? 'flex flex-col border-t border-hairline p-4 lg:border-t-0 lg:border-l'
		: variant === 'paper'
			? 'flex h-[394px] w-[410px] shrink-0 flex-col gap-4 rounded-lg border border-hairline bg-surface p-[18px] [font-synthesis:none]'
			: 'flex flex-col'}
>
	{#if variant === 'paper'}
		<div class="flex h-[18px] shrink-0 items-center gap-2.5">
			<h3 class="text-[15px] leading-[18px] font-semibold">Manual control</h3>
			<span
				class="rounded-full bg-raised px-2 py-[3px] font-mono text-[10px] leading-3 tracking-[0.08em] text-text-muted"
				>WEBRTC</span
			>
		</div>
		<div class="mx-auto grid h-36 w-[168px] shrink-0 grid-cols-3 grid-rows-3 gap-1.5">
			{#each directions as direction (direction.label)}
				{#if direction.movement}
					<button
						type="button"
						class="grid h-11 w-[52px] place-items-center rounded-sm border border-hairline-strong bg-raised text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-45 {direction.position}"
						aria-label={direction.label}
						disabled={!commandAvailable}
						onpointerdown={(event) => handlePointerDown(event, direction.movement)}
						onpointerup={handlePointerEnd}
						onpointercancel={handlePointerEnd}
						onkeydown={(event) => handlePtzKeydown(event, direction.movement)}
						onkeyup={handlePtzKeyup}
					>
						<direction.icon class="size-4" />
					</button>
				{:else}
					<button
						type="button"
						class="grid h-11 w-[52px] place-items-center rounded-sm border border-hairline-strong bg-primary text-on-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-45 {direction.position}"
						aria-label={direction.label}
						disabled={!commandAvailable}
						onclick={() => void stopPtz()}
					>
						<direction.icon class="size-4" />
					</button>
				{/if}
			{/each}
		</div>
		<div class="grid h-9 shrink-0 grid-cols-[44px_156px_156px] items-center gap-2">
			<span class="font-mono text-[10px] leading-3 tracking-[0.1em] text-text-faint">ZOOM</span>
			<button
				type="button"
				class="inline-flex h-9 items-center justify-center gap-2 rounded-sm border border-hairline-strong bg-raised text-xs font-medium disabled:cursor-not-allowed disabled:opacity-45"
				aria-label="Zoom out"
				onpointerdown={(event) => handlePointerDown(event, { pan: 0, tilt: 0, zoom: -1 })}
				onpointerup={handlePointerEnd}
				onpointercancel={handlePointerEnd}
				onkeydown={(event) => handlePtzKeydown(event, { pan: 0, tilt: 0, zoom: -1 })}
				onkeyup={handlePtzKeyup}
				disabled={!commandAvailable}
			>
				<MinusIcon class="size-[15px]" />Wide
			</button>
			<button
				type="button"
				class="inline-flex h-9 items-center justify-center gap-2 rounded-sm border border-hairline-strong bg-raised text-xs font-medium disabled:cursor-not-allowed disabled:opacity-45"
				aria-label="Zoom in"
				onpointerdown={(event) => handlePointerDown(event, { pan: 0, tilt: 0, zoom: 1 })}
				onpointerup={handlePointerEnd}
				onpointercancel={handlePointerEnd}
				onkeydown={(event) => handlePtzKeydown(event, { pan: 0, tilt: 0, zoom: 1 })}
				onkeyup={handlePtzKeyup}
				disabled={!commandAvailable}
			>
				<PlusIcon class="size-[15px]" />Tele
			</button>
		</div>
		<div class="flex h-[14px] shrink-0 items-center gap-2">
			<span class="w-11 font-mono text-[10px] leading-3 tracking-[0.1em] text-text-faint"
				>SPEED</span
			>
			<span class="h-1 flex-1 rounded-full bg-hairline"
				><span class="block h-1 w-3/5 rounded-full bg-primary"></span></span
			>
			<span class="w-[42px] font-mono text-[10px] leading-3 text-text-faint">FIXED</span>
		</div>
		{#if reason}<p class="text-center font-mono text-2xs tracking-caps text-activity">
				{reason}
			</p>{/if}
		{#if ptzError}<p class="text-2xs text-destructive" role="alert">{ptzError}</p>{/if}
	{:else}
		{#if variant === 'desktop'}
			<div>
				<h3 class="text-sm font-semibold">PTZ</h3>
				<p class="mt-1 text-xs leading-5 text-text-muted">
					Steering is available only with explicit camera and command-transport authority.
				</p>
			</div>
		{/if}
		<div class={variant === 'mobile' ? 'grid h-[150px] grid-cols-[204px_140px] gap-[14px]' : ''}>
			<div
				class="mx-auto {variant === 'desktop' ? 'mt-5' : ''} grid grid-cols-3 grid-rows-3 gap-1.5"
			>
				{#each directions as direction (direction.label)}
					{#if direction.movement}
						<button
							type="button"
							class="grid place-items-center rounded-sm border border-hairline-strong bg-raised text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-45 {variant ===
							'mobile'
								? 'size-12'
								: 'size-11'} {direction.position}"
							aria-label={direction.label}
							disabled={!commandAvailable}
							onpointerdown={(event) => handlePointerDown(event, direction.movement)}
							onpointerup={handlePointerEnd}
							onpointercancel={handlePointerEnd}
							onkeydown={(event) => handlePtzKeydown(event, direction.movement)}
							onkeyup={handlePtzKeyup}
						>
							<direction.icon class="size-4" />
						</button>
					{:else}
						<button
							type="button"
							class="grid place-items-center rounded-sm border border-hairline-strong bg-primary text-on-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-45 {variant ===
							'mobile'
								? 'size-12'
								: 'size-11'} {direction.position}"
							aria-label={direction.label}
							disabled={!commandAvailable}
							onclick={() => void stopPtz()}
						>
							<direction.icon class="size-4" />
						</button>
					{/if}
				{/each}
			</div>
			<div class={variant === 'mobile' ? 'flex flex-col gap-3' : 'mt-4 flex justify-center gap-2'}>
				<button
					type="button"
					class="inline-flex h-9 items-center justify-center gap-1.5 rounded-sm border border-hairline-strong bg-raised px-3 text-xs disabled:cursor-not-allowed disabled:opacity-45"
					aria-label="Zoom out"
					onpointerdown={(event) => handlePointerDown(event, { pan: 0, tilt: 0, zoom: -1 })}
					onpointerup={handlePointerEnd}
					onpointercancel={handlePointerEnd}
					onkeydown={(event) => handlePtzKeydown(event, { pan: 0, tilt: 0, zoom: -1 })}
					onkeyup={handlePtzKeyup}
					disabled={!commandAvailable}><MinusIcon class="size-3.5" />Zoom out</button
				>
				<button
					type="button"
					class="inline-flex h-9 items-center justify-center gap-1.5 rounded-sm border border-hairline-strong bg-raised px-3 text-xs disabled:cursor-not-allowed disabled:opacity-45"
					aria-label="Zoom in"
					onpointerdown={(event) => handlePointerDown(event, { pan: 0, tilt: 0, zoom: 1 })}
					onpointerup={handlePointerEnd}
					onpointercancel={handlePointerEnd}
					onkeydown={(event) => handlePtzKeydown(event, { pan: 0, tilt: 0, zoom: 1 })}
					onkeyup={handlePtzKeyup}
					disabled={!commandAvailable}><PlusIcon class="size-3.5" />Zoom in</button
				>
			</div>
		</div>
		{#if reason}
			<p class="pt-3 text-center font-mono text-2xs tracking-caps text-activity">{reason}</p>
		{/if}
		<div class={variant === 'desktop' ? 'mt-4 border-t border-hairline pt-3' : 'mt-[14px]'}>
			<p class="text-sm leading-5 font-semibold">Presets</p>
			{#if !commandAvailable}
				<p class="mt-1 text-2xs leading-4 text-text-muted">
					Preset list unavailable without the browser command transport.
				</p>
			{:else if loadingPresets}
				<p class="mt-1 text-2xs leading-4 text-text-muted">Loading presets…</p>
			{:else if presets.length === 0}
				<p class="mt-1 text-2xs leading-4 text-text-muted">No presets reported.</p>
			{:else}
				<div class="mt-2 grid grid-cols-3 gap-1.5">
					{#each presets.slice(0, 3) as preset (preset.id)}
						<button
							type="button"
							class="h-[58px] rounded-sm border border-hairline-strong bg-raised px-2 text-xs {preset ===
							presets[0]
								? 'border-primary'
								: ''}"
							onclick={() => void gotoPreset(preset)}
						>
							{preset.name}
						</button>
					{/each}
				</div>
			{/if}
			{#if ptzError}<p class="mt-2 text-2xs text-destructive" role="alert">{ptzError}</p>{/if}
		</div>
	{/if}
</div>
