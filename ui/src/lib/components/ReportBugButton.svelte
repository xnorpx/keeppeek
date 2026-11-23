<script lang="ts">
	import { Button } from '$lib/components/ui/button/index.js';
	import { useReportContext } from '$lib/report-context';

	const report = useReportContext();
</script>

{#if report?.authorized()}
	<Button variant="outline" onclick={report.show}>Report a bug</Button>
{/if}
