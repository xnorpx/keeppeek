<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import DesktopCameraDiagnosis from '$lib/components/DesktopCameraDiagnosis.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import { cameraDiagnosisEvidence } from '$lib/health-presentation';
	import { diagnosisVisualHealth } from '../../e2e/fixtures/diagnosis';

	setCapabilityState([]);
	const evidence = cameraDiagnosisEvidence(diagnosisVisualHealth, 'back-yard');
	if (!evidence) throw new Error('Board 30 camera evidence is unavailable');
</script>

<main
	data-paper-scenario="health.desktop.camera-diagnosis"
	class="flex h-[776px] w-[1440px] shrink-0 overflow-hidden rounded-lg border border-hairline bg-ground [font-synthesis:none]"
>
	<DesktopPaperRail active="health" />
	<section class="h-[774px] w-[1374px] shrink-0" aria-label="Camera diagnosis">
		<DesktopCameraDiagnosis {evidence} />
	</section>
</main>
