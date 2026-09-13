<script lang="ts">
	import * as Sheet from './ui/sheet/index.js';
	import { Button } from './ui/button/index.js';
	import KeepCameraSwitcher from './KeepCameraSwitcher.svelte';
	import CopyMomentLink from './CopyMomentLink.svelte';
	import { recordedPlaybackRates } from './RecordedPlaybackControls.svelte';
	import type { CameraListItem } from '$lib/types';
	import type { RecordedQualityPreference } from '$lib/recorded-playback-policy';
	import CalendarIcon from '@lucide/svelte/icons/calendar-days';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import SettingsIcon from '@lucide/svelte/icons/sliders-horizontal';
	import RefreshIcon from '@lucide/svelte/icons/refresh-cw';

	type Props = {
		compact: boolean;
		cameras: CameraListItem[];
		cameraId: string;
		switching: boolean;
		dates: string[];
		date: string;
		olderDate: string | null;
		newerDate: string | null;
		panel: 'playback' | 'camera-date' | null;
		returnFocus: HTMLElement | null;
		volume: number;
		rate: number;
		quality: RecordedQualityPreference;
		qualityOptions: ReadonlyArray<{ value: RecordedQualityPreference; label: string }>;
		mediaDisabled: boolean;
		loading: boolean;
		copyDisabled: boolean;
		getLink: () => string | null;
		formatDate: (date: string) => string;
		onpanel: (panel: 'playback' | 'camera-date') => void;
		onclose: () => void;
		oncamera: (cameraId: string, direction: -1 | 1) => void;
		ondate: (date: string) => void;
		onvolume: (volume: number) => void;
		onrate: (rate: number) => void;
		onquality: (event: Event) => void;
		onrefresh: () => void;
	};
	let {
		compact,
		cameras,
		cameraId,
		switching,
		dates,
		date,
		olderDate,
		newerDate,
		panel,
		returnFocus,
		volume,
		rate,
		quality,
		qualityOptions,
		mediaDisabled,
		loading,
		copyDisabled,
		getLink,
		formatDate,
		onpanel,
		onclose,
		oncamera,
		ondate,
		onvolume,
		onrate,
		onquality,
		onrefresh
	}: Props = $props();
	let camera = $derived(cameras.find((candidate) => candidate.id === cameraId));
</script>

{#if compact}
	<div class="flex min-w-0 items-center gap-1 px-2" data-keep-mobile-controls>
		<Button
			variant="ghost"
			class="h-11 min-w-0 flex-1 justify-start gap-2 px-2"
			aria-label={`Camera and date, ${camera?.name ?? cameraId}`}
			onclick={() => onpanel('camera-date')}
		>
			<CameraIcon class="size-4 shrink-0" /><span class="truncate"
				>{camera?.name ?? 'Choose camera'}</span
			>
		</Button>
		<Button
			variant="ghost"
			class="h-11 min-w-11 gap-2 px-2"
			aria-label={`Recorded day, ${date ? formatDate(date) : 'none available'}`}
			onclick={() => onpanel('camera-date')}
		>
			<CalendarIcon class="size-4" /><span class="font-mono text-xs"
				>{date ? date.slice(5) : 'Date'}</span
			>
		</Button>
		<Button
			variant="ghost"
			class="size-11 shrink-0"
			aria-label="Playback options"
			onclick={() => onpanel('playback')}><SettingsIcon class="size-4" /></Button
		>
	</div>
{/if}

<Sheet.Root
	open={panel !== null}
	onOpenChange={(open) => {
		if (!open) onclose();
	}}
>
	<Sheet.Content
		side="bottom"
		class="max-h-[85dvh] gap-0 overflow-y-auto rounded-t-lg border-hairline bg-surface p-0 text-foreground motion-reduce:animate-none [&>button:last-child]:top-2 [&>button:last-child]:right-2 [&>button:last-child]:grid [&>button:last-child]:size-11 [&>button:last-child]:place-items-center"
		onCloseAutoFocus={(event) => {
			event.preventDefault();
			const target = returnFocus?.isConnected
				? returnFocus
				: document.querySelector<HTMLElement>('[data-keep-mode-switcher] [aria-pressed="true"]');
			target?.focus({ preventScroll: true });
		}}
	>
		<header class="border-b border-hairline px-4 py-4 pr-16">
			<Sheet.Title>{panel === 'camera-date' ? 'Camera and date' : 'Playback options'}</Sheet.Title>
			<Sheet.Description class="mt-1 text-xs text-text-muted"
				>Changes apply immediately. Playback stays open.</Sheet.Description
			>
		</header>
		<div class="space-y-4 p-4">
			{#if panel === 'camera-date'}
				<KeepCameraSwitcher
					{cameras}
					selectedCameraId={cameraId}
					{switching}
					touch
					onselect={oncamera}
				/>
				<div class="space-y-1">
					<label for="mobile-recorded-day" class="text-sm font-medium">Recorded day</label>
					<div class="flex min-w-0 items-center gap-2">
						<Button
							variant="outline"
							class="size-11 shrink-0"
							aria-label="Previous recorded day"
							disabled={!olderDate || loading}
							onclick={() => olderDate && ondate(olderDate)}
							><ChevronLeftIcon class="size-4" /></Button
						>
						<select
							id="mobile-recorded-day"
							class="h-11 min-w-0 flex-1 rounded-sm border border-input bg-background px-2 text-sm"
							value={date}
							disabled={dates.length === 0 || loading}
							onchange={(event) => ondate(event.currentTarget.value)}
						>
							{#each dates as day (day)}<option value={day}>{formatDate(day)}</option>{/each}
						</select>
						<Button
							variant="outline"
							class="size-11 shrink-0"
							aria-label="Next recorded day"
							disabled={!newerDate || loading}
							onclick={() => newerDate && ondate(newerDate)}
							><ChevronRightIcon class="size-4" /></Button
						>
					</div>
					{#if !newerDate}<p class="text-xs text-text-muted">
							{dates.length
								? 'No later recorded day is available.'
								: 'No recorded days are available for this camera.'}
						</p>{/if}
				</div>
			{:else}
				<label class="grid gap-1 text-sm"
					>Recording volume<input
						type="range"
						class="h-11 w-full accent-primary"
						min="0"
						max="1"
						step="0.05"
						value={volume}
						disabled={mediaDisabled}
						oninput={(event) => onvolume(Number(event.currentTarget.value))}
					/></label
				>
				<label class="grid gap-1 text-sm"
					>Playback speed<select
						class="h-11 rounded-sm border border-input bg-background px-2"
						value={rate}
						disabled={mediaDisabled}
						onchange={(event) => onrate(Number(event.currentTarget.value))}
					>
						{#if !recordedPlaybackRates.includes(rate)}<option value={rate}>{rate}x</option>{/if}
						{#each recordedPlaybackRates as speed (speed)}<option value={speed}>{speed}x</option
							>{/each}
					</select></label
				>
				<label class="grid gap-1 text-sm"
					>Quality<select
						class="h-11 rounded-sm border border-input bg-background px-2"
						value={quality}
						disabled={qualityOptions.length === 0}
						onchange={onquality}
					>
						{#each qualityOptions as option (option.value)}<option value={option.value}
								>{option.label}</option
							>{/each}
					</select></label
				>
				<div class="flex items-center gap-3">
					<CopyMomentLink {getLink} disabled={copyDisabled} /><span class="text-sm"
						>Copy link to this moment</span
					>
				</div>
				<Button
					variant="outline"
					class="min-h-11 w-full justify-start gap-2"
					disabled={!cameraId || loading}
					onclick={onrefresh}><RefreshIcon class="size-4" />Refresh recordings</Button
				>
			{/if}
		</div>
		<footer class="border-t border-hairline p-4 pb-[max(1rem,env(safe-area-inset-bottom))]">
			<Button class="h-11 w-full" onclick={onclose}>Done</Button>
		</footer>
	</Sheet.Content>
</Sheet.Root>
