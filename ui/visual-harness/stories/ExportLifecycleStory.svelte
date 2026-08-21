<script lang="ts">
	import type { MediaExportJob } from '$lib/control-client';
	import ExportLifecycleCard from './ExportLifecycleCard.svelte';

	const startMs = Date.parse('2026-08-18T06:11:48Z');
	const endMs = Date.parse('2026-08-18T06:14:20Z');
	const baseJob: Omit<MediaExportJob, 'id' | 'status'> = {
		sourceId: 'back-yard',
		streamId: 'main',
		requestedStartMs: startMs,
		requestedEndMs: endMs,
		alignedStartMs: startMs,
		progress: 0,
		bytesWritten: 0,
		estimatedBytes: 118_000_000,
		fileName: null,
		sha256: null,
		expiresAtMs: null,
		missingRanges: [],
		error: null,
		retryable: false,
		burnInTimestamp: false
	};
	const checksum = `a41f9c2e${'0'.repeat(48)}7d1304b8`;
	const jobs: MediaExportJob[] = [
		{
			...baseJob,
			id: 'export-running',
			status: 'running',
			progress: 0.62,
			bytesWritten: 74_000_000
		},
		{
			...baseJob,
			id: 'export-ready',
			status: 'ready',
			progress: 1,
			bytesWritten: 118_000_000,
			fileName: 'back-yard_2026-08-18T06-11-48Z_152s.mp4',
			sha256: checksum,
			expiresAtMs: Date.now() + (23 * 60 + 42) * 60_000
		},
		{
			...baseJob,
			id: 'export-partial',
			status: 'partial',
			missingRanges: [
				{
					startMs: Date.parse('2026-08-18T06:12:55Z'),
					endMs: Date.parse('2026-08-18T06:13:24Z')
				}
			]
		},
		{
			...baseJob,
			id: 'export-failed',
			status: 'failed',
			progress: 0.41,
			error: 'export.write: no space left on device (/mnt/keeppeek/tmp)',
			retryable: true
		}
	];
</script>

<main
	data-paper-scenario="keep.desktop.export-lifecycle"
	class="[font-synthesis:none]"
	style="display: flex; width: 1440px; height: 369px; gap: 24px; overflow: hidden; background: var(--color-ground);"
>
	{#each jobs as job (job.id)}
		<div style="width: 342px; flex-shrink: 0;">
			<ExportLifecycleCard {job} />
		</div>
	{/each}
</main>
