<script lang="ts">
	import {
		maximumWallDecorationPx,
		peekWallAppearancePreset,
		peekWallAppearancePresets,
		type PeekWallPreferences
	} from '$lib/peek-wall-preferences';

	let {
		preferences,
		onchange
	}: {
		preferences: PeekWallPreferences;
		onchange: (preferences: PeekWallPreferences) => void;
	} = $props();
	const id = $props.id();
	const dimensions = [
		{ field: 'gapPx', label: 'Gap' },
		{ field: 'cornerRadiusPx', label: 'Corner radius' }
	] as const;
	let selectedPreset = $derived(peekWallAppearancePreset(preferences));

	function setDimension(event: Event, field: 'gapPx' | 'cornerRadiusPx'): void {
		const input = event.currentTarget as HTMLInputElement;
		if (input.validity.valid && Number.isInteger(input.valueAsNumber)) {
			onchange({ ...preferences, [field]: input.valueAsNumber });
		}
	}
</script>

<fieldset class="space-y-3">
	<legend class="mb-1.5 text-xs font-medium text-muted-foreground">
		Appearance {#if selectedPreset === 'custom'}<span class="ml-2 text-foreground">Custom</span
			>{/if}
	</legend>
	<div class="flex gap-1">
		{#each peekWallAppearancePresets as preset (preset.id)}
			<label
				class="flex min-h-11 min-w-0 flex-1 cursor-pointer items-center justify-center gap-1.5 rounded-sm border border-input px-1.5 text-xs has-checked:border-primary has-checked:bg-accent has-focus-visible:ring-2 has-focus-visible:ring-ring"
			>
				<input
					type="radio"
					name={`${id}-preset`}
					checked={selectedPreset === preset.id}
					onchange={() =>
						onchange({
							...preferences,
							gapPx: preset.gapPx,
							cornerRadiusPx: preset.cornerRadiusPx
						})}
					class="accent-primary"
				/>
				{preset.label}
			</label>
		{/each}
	</div>
	{#each dimensions as dimension (dimension.field)}
		<div>
			<label for={`${id}-${dimension.field}`} class="text-xs font-medium">{dimension.label}</label>
			<div class="flex items-center gap-3">
				<input
					id={`${id}-${dimension.field}`}
					type="range"
					min="0"
					max={maximumWallDecorationPx}
					step="1"
					value={preferences[dimension.field]}
					oninput={(event) => setDimension(event, dimension.field)}
					aria-valuetext={`${preferences[dimension.field]} px`}
					class="h-11 min-w-0 flex-1 accent-primary"
				/>
				<input
					type="number"
					min="0"
					max={maximumWallDecorationPx}
					step="1"
					aria-label={`${dimension.label} (px)`}
					value={preferences[dimension.field]}
					oninput={(event) => setDimension(event, dimension.field)}
					class="h-11 w-16 rounded-sm border border-input bg-background px-2 font-mono text-xs focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				/>
				<span aria-hidden="true" class="text-xs text-muted-foreground">px</span>
			</div>
		</div>
	{/each}
</fieldset>
