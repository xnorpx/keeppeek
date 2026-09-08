<script lang="ts">
	import { onMount } from 'svelte';
	import { Popover } from 'bits-ui';
	import Settings2Icon from '@lucide/svelte/icons/settings-2';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SaveIcon from '@lucide/svelte/icons/save';
	import PeekWallAppearance from './PeekWallAppearance.svelte';
	import {
		defaultPeekWallPreferences,
		maximumWallStreams,
		type PeekWallPreferences
	} from '$lib/peek-wall-preferences';
	import { PeekWakeLock, type PeekWakeLockState } from '$lib/peek-wake-lock';

	type Props = {
		preferences: PeekWallPreferences;
		deviceCapacity: number;
		visibleStreams: number;
		activeStreams: number;
		editable?: boolean;
		dirty?: boolean;
		saving?: boolean;
		saveError?: string | null;
		dashboardName?: string;
		onchange: (preferences: PeekWallPreferences) => void;
		onsave?: () => void;
		ondiscard?: () => void;
	};
	let {
		preferences,
		deviceCapacity,
		visibleStreams,
		activeStreams,
		editable = false,
		dirty = false,
		saving = false,
		saveError = null,
		dashboardName,
		onchange,
		onsave,
		ondiscard
	}: Props = $props();
	const id = $props.id();
	let open = $state(false);
	let wakeState = $state<PeekWakeLockState>('off');
	let wakeLock: PeekWakeLock | null = null;
	const wakeLabels: Record<PeekWakeLockState, string> = {
		off: 'Off',
		'awaiting-gesture': 'Waiting for interaction',
		requesting: 'Requesting',
		active: 'Active',
		released: 'Released',
		unsupported: 'Unsupported in this browser or connection',
		denied: 'Denied by browser or device',
		'release-failed': 'Release failed'
	};
	let capacity = $derived(Math.min(deviceCapacity, preferences.streamLimit));
	let overBudget = $derived(Math.max(0, visibleStreams - capacity));

	$effect(() => {
		void wakeLock?.setEnabled(preferences.keepAwake);
	});

	onMount(() => {
		const request =
			window.isSecureContext && navigator.wakeLock
				? () => navigator.wakeLock.request('screen')
				: null;
		wakeLock = new PeekWakeLock(request, (value) => (wakeState = value));
		const visibilityChanged = () => {
			void wakeLock?.setVisible(document.visibilityState === 'visible');
		};
		const activate = (event: Event) => {
			if (event.isTrusted) void wakeLock?.activate();
		};
		const pageHidden = () => void wakeLock?.setVisible(false);
		visibilityChanged();
		void wakeLock.setEnabled(preferences.keepAwake);
		document.addEventListener('visibilitychange', visibilityChanged);
		document.addEventListener('pointerdown', activate);
		document.addEventListener('keydown', activate);
		window.addEventListener('pagehide', pageHidden);
		window.addEventListener('pageshow', visibilityChanged);
		return () => {
			document.removeEventListener('visibilitychange', visibilityChanged);
			document.removeEventListener('pointerdown', activate);
			document.removeEventListener('keydown', activate);
			window.removeEventListener('pagehide', pageHidden);
			window.removeEventListener('pageshow', visibilityChanged);
			void wakeLock?.dispose();
		};
	});

	function update(patch: Partial<PeekWallPreferences>): void {
		onchange({ ...preferences, ...patch });
	}

	function setStreamLimit(event: Event): void {
		const input = event.currentTarget as HTMLInputElement;
		if (input.validity.valid && Number.isInteger(input.valueAsNumber)) {
			update({ streamLimit: input.valueAsNumber });
		}
	}

	function setKeepAwake(event: Event): void {
		const enabled = (event.currentTarget as HTMLInputElement).checked;
		void wakeLock?.setEnabled(enabled, true);
		update({ keepAwake: enabled });
	}

	function save(event: SubmitEvent): void {
		event.preventDefault();
		if (editable && dirty && !saving) onsave?.();
	}
</script>

<div data-peek-wall-settings class="absolute top-3 right-3 z-40">
	<Popover.Root bind:open>
		<Popover.Trigger
			class="group/wall-settings relative flex size-11 items-start justify-center text-white/90 focus-visible:outline-none"
			aria-label="Wall display settings"
			title="Wall display settings"
		>
			<span
				data-wall-settings-frame
				class="grid size-8 place-items-center rounded-sm bg-video/70 shadow-md ring-1 ring-white/10 backdrop-blur-md group-hover/wall-settings:bg-video/90 group-focus-visible/wall-settings:ring-2 group-focus-visible/wall-settings:ring-primary"
			>
				<Settings2Icon class="size-3.5" />
			</span>
			{#if wakeState === 'active'}
				<span class="absolute top-1.5 right-1.5 size-1.5 rounded-full bg-live"></span>
			{/if}
		</Popover.Trigger>
		<Popover.Portal>
			<Popover.Content
				aria-label="Wall display settings"
				side="bottom"
				align="end"
				sideOffset={8}
				collisionPadding={12}
				class="z-50 flex max-h-[min(calc(100dvh-6rem),var(--bits-popover-content-available-height))] w-80 max-w-[calc(100vw-1.5rem)] flex-col gap-4 overflow-hidden rounded-md border border-hairline-strong bg-popover p-4 text-sm text-popover-foreground shadow-xl"
			>
				<h2 class="text-base font-semibold">Wall display</h2>
				{#if dashboardName}<p class="truncate text-xs text-muted-foreground">
						{dashboardName}
					</p>{/if}
				<form onsubmit={save} class="flex min-h-0 flex-1 flex-col gap-3">
					<div class="min-h-0 flex-1 overflow-y-auto pr-1">
						<fieldset disabled={!editable || saving} class="min-w-0 space-y-4 disabled:opacity-60">
							<legend class="sr-only">Dashboard display settings</legend>
							<PeekWallAppearance {preferences} {onchange} />
							<fieldset>
								<legend class="mb-1.5 text-xs font-medium text-muted-foreground">Tile shape</legend>
								<div class="options">
									{#each ['16:9', '4:3', 'native'] as shape}
										<label>
											<input
												type="radio"
												name={`${id}-shape`}
												checked={preferences.tileShape === shape}
												onchange={() =>
													update({ tileShape: shape as PeekWallPreferences['tileShape'] })}
											/>
											{shape === 'native' ? 'Native' : shape}
										</label>
									{/each}
								</div>
							</fieldset>
							<fieldset>
								<legend class="mb-1.5 text-xs font-medium text-muted-foreground">Media fit</legend>
								<div class="options">
									{#each ['contain', 'cover'] as fit}
										<label>
											<input
												type="radio"
												name={`${id}-fit`}
												checked={preferences.mediaFit === fit}
												onchange={() =>
													update({ mediaFit: fit as PeekWallPreferences['mediaFit'] })}
											/>
											{fit === 'contain' ? 'Contain' : 'Cover (cropped)'}
										</label>
									{/each}
								</div>
							</fieldset>
							<fieldset>
								<legend class="mb-1.5 text-xs font-medium text-muted-foreground"
									>Streaming mode</legend
								>
								<div id={`${id}-demand`} class="mb-2 space-y-1 font-mono text-xs">
									<p>{visibleStreams} streams / {visibleStreams} decoders requested</p>
									<p class="text-muted-foreground">Budget {capacity} / Active {activeStreams}</p>
									{#if overBudget > 0}<p class="text-activity">{overBudget} over budget</p>{/if}
								</div>
								<div class="options">
									{#each ['smart', 'continuous'] as mode}
										<label>
											<input
												type="radio"
												name={`${id}-mode`}
												aria-describedby={`${id}-demand`}
												checked={preferences.streamingMode === mode}
												onchange={() =>
													update({ streamingMode: mode as PeekWallPreferences['streamingMode'] })}
											/>
											{mode === 'smart' ? 'Smart' : 'Continuous'}
										</label>
									{/each}
								</div>
							</fieldset>
							<label class="flex items-center justify-between gap-3">
								Stream limit
								<input
									type="number"
									min="1"
									max={maximumWallStreams}
									step="1"
									value={preferences.streamLimit}
									onchange={setStreamLimit}
									class="h-11 w-20 rounded-sm border border-input bg-background px-2 font-mono focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								/>
							</label>
							<div class="border-t border-hairline pt-3">
								<label class="flex min-h-11 items-center justify-between gap-3">
									Keep display awake
									<input
										type="checkbox"
										role="switch"
										checked={preferences.keepAwake}
										disabled={wakeState === 'unsupported' && !preferences.keepAwake}
										onchange={setKeepAwake}
										class="size-5 accent-primary"
									/>
								</label>
								<p
									data-wake-lock-state={wakeState}
									role="status"
									class="text-xs text-muted-foreground"
								>
									Wake lock: {wakeLabels[wakeState]}
								</p>
							</div>
							<button
								type="button"
								class="flex min-h-11 w-full items-center justify-center gap-2 rounded-sm border border-input hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								onclick={() => {
									void wakeLock?.setEnabled(false);
									onchange(defaultPeekWallPreferences());
								}}
							>
								<RefreshCwIcon class="size-4" /> Reset wall settings
							</button>
						</fieldset>
					</div>
					<div class="shrink-0 space-y-3 border-t border-hairline pt-3">
						{#if saveError}<p role="alert" class="text-xs text-destructive">{saveError}</p>{/if}
						{#if editable}
							<p role="status" class="text-xs text-muted-foreground">
								{saving ? 'Saving to server...' : dirty ? 'Unsaved changes' : 'Saved on server'}
							</p>
							<div class="flex gap-2">
								<button
									type="button"
									onclick={ondiscard}
									disabled={saving || (!dirty && !saveError)}
									class="min-h-11 flex-1 rounded-sm border border-input px-3 text-xs hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50"
									>Discard changes</button
								>
								<button
									type="submit"
									disabled={saving || !dirty}
									aria-label="Save display settings"
									class="flex min-h-11 flex-1 items-center justify-center gap-2 rounded-sm bg-primary px-3 text-xs font-medium text-primary-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50"
									><SaveIcon class="size-4" />Save</button
								>
							</div>
						{:else}
							<p class="text-xs text-muted-foreground">Read-only dashboard</p>
						{/if}
					</div>
				</form>
			</Popover.Content>
		</Popover.Portal>
	</Popover.Root>
</div>

<style>
	.options {
		display: flex;
		gap: 0.25rem;
	}
	.options label {
		display: flex;
		min-height: 2.75rem;
		min-width: 0;
		flex: 1;
		align-items: center;
		justify-content: center;
		gap: 0.375rem;
		border: 1px solid var(--color-input);
		border-radius: 0.25rem;
		padding-inline: 0.375rem;
		font-size: 0.75rem;
		cursor: pointer;
	}
	.options label:has(:checked) {
		border-color: var(--color-primary);
		background: var(--color-accent);
	}
	.options label:focus-within {
		outline: 2px solid var(--color-ring);
		outline-offset: 2px;
	}
	.options input {
		accent-color: var(--color-primary);
	}
</style>
