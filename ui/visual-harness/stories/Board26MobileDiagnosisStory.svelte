<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import MobileCameraDiagnosis from '$lib/components/MobileCameraDiagnosis.svelte';
	import { cameraDiagnosisEvidence } from '$lib/health-presentation';
	import { diagnosisVisualHealth } from '../../e2e/fixtures/diagnosis';
	import MobileDeviceStatusBar from './MobileDeviceStatusBar.svelte';

	type State = 'issue' | 'stream';
	type Props = { state: State };

	let { state }: Props = $props();
	setCapabilityState(['keeppeek.runtime-config.v1']);

	const scenarioIds: Record<State, string> = {
		issue: 'health.mobile.camera-issue',
		stream: 'health.mobile.stream-evidence'
	};
	let evidence = $derived(
		cameraDiagnosisEvidence(diagnosisVisualHealth, state === 'issue' ? 'back-yard' : 'porch')
	);
</script>

<main
	data-paper-scenario={scenarioIds[state]}
	class="flex h-[844px] w-[390px] flex-col overflow-hidden rounded-lg border border-hairline-strong bg-ground [font-synthesis:none]"
>
	<MobileDeviceStatusBar />
	{#if evidence}
		<MobileCameraDiagnosis
			{evidence}
			generatedAtMs={diagnosisVisualHealth.generated_at_ms}
			actionFixed={false}
		/>
	{/if}
</main>
