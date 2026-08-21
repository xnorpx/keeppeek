<script lang="ts">
	import { onMount } from 'svelte';
	import CameraFleetSkeleton from '$lib/components/CameraFleetSkeleton.svelte';
	import ColdSeekState from '$lib/components/ColdSeekState.svelte';
	import DiscoveryProgressState from '$lib/components/DiscoveryProgressState.svelte';
	import EventNoResultsState from '$lib/components/EventNoResultsState.svelte';
	import FirstKeyframeState from '$lib/components/FirstKeyframeState.svelte';
	import SettingsApplyingState from '$lib/components/SettingsApplyingState.svelte';

	type State =
		'applying' | 'cold-seek' | 'discovery' | 'first-keyframe' | 'fleet-skeleton' | 'no-results';

	let { state }: { state: State } = $props();

	const scenarioIds: Record<State, string> = {
		applying: 'settings.waiting.applying',
		'cold-seek': 'keep.waiting.cold-seek',
		discovery: 'cameras.waiting.discovery',
		'first-keyframe': 'peek.waiting.first-keyframe',
		'fleet-skeleton': 'cameras.waiting.fleet-skeleton',
		'no-results': 'events.empty.no-results'
	};

	onMount(() => {
		const root = document.documentElement;
		const previousTheme = root.dataset.theme;
		const wasDark = root.classList.contains('dark');
		root.classList.add('dark');
		root.dataset.theme = 'dark';
		return () => {
			root.classList.toggle('dark', wasDark);
			if (previousTheme === undefined) delete root.dataset.theme;
			else root.dataset.theme = previousTheme;
		};
	});
</script>

<main
	data-paper-scenario={scenarioIds[state]}
	class="w-[462px] overflow-hidden [font-synthesis:none]"
>
	{#if state === 'first-keyframe'}
		<FirstKeyframeState label="Alley" elapsedMs={400} class="h-[172px] w-[462px]" />
	{:else if state === 'cold-seek'}
		<ColdSeekState
			timestampLabel="14 Aug · 22:41:07"
			timestampLines={['14 Aug ·', '22:41:07']}
			elapsedMs={1_200}
			activityLabel="Reading from the long-term tier"
			class="h-[172px] w-[462px]"
		/>
	{:else if state === 'discovery'}
		<DiscoveryProgressState
			answeredCount={7}
			elapsedMs={3_200}
			probesSent={143}
			totalProbes={200}
			progressOverride={262 / 426}
			class="h-[172px] w-[462px]"
		/>
	{:else if state === 'no-results'}
		<EventNoResultsState
			clauses={[
				{ label: 'camera:workshop' },
				{ label: 'type:vehicle' },
				{ label: 'confidence:>0.9', constraining: true },
				{ label: '14 Aug' }
			]}
			title="No vehicles on Workshop that day"
			description="There were 41 events on Workshop on 14 August, and 4 of them were vehicles — none scored above 0.9. Confidence is the filter that emptied this, and it is the one marked above."
			suggestionLabel="Drop to >0.5 · 4 results"
			onloosen={() => {}}
			onclear={() => {}}
			class="h-[238px] w-[462px]"
		/>
	{:else if state === 'applying'}
		<SettingsApplyingState
			fieldLabel="Transport"
			confirmedValue="TCP"
			detail="Restarting the worker for Back Yard. Its recording pauses for about two seconds and the gap will be visible in Keep — no other camera is touched."
			class="h-[238px] w-[462px]"
		/>
	{:else}
		<CameraFleetSkeleton
			cameraCount={42}
			compact
			statusLabel="Reading the catalog · 42 cameras"
			class="h-[238px] w-[462px]"
		/>
	{/if}
</main>
