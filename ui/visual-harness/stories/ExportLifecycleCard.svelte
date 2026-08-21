<script lang="ts">
	import { untrack } from 'svelte';
	import { setCapabilityState } from '$lib/capability-context';
	import KeepExportPanel from '$lib/components/KeepExportPanel.svelte';
	import { setControlClient } from '$lib/control-context';
	import { ControlClient, type MediaExportJob } from '$lib/control-client';
	import type { RecordingSegment } from '$lib/types';

	class ExportStoryClient extends ControlClient {
		private readonly job: MediaExportJob;

		constructor(job: MediaExportJob) {
			super();
			this.job = job;
		}

		override async listExports(): Promise<MediaExportJob[]> {
			return [this.job];
		}

		override async getExport(): Promise<MediaExportJob> {
			return this.job;
		}
	}

	let { job }: { job: MediaExportJob } = $props();
	setControlClient(new ExportStoryClient(untrack(() => job)));
	setCapabilityState(['keeppeek.media-export.v1']);

	const segment: RecordingSegment = {
		stream: 'main',
		date: '2026-08-18',
		hour: '06',
		filename: 'back-yard.mp4',
		url: '/story/export/back-yard.mp4',
		start_time_ms: Date.parse('2026-08-18T06:11:48Z'),
		end_time_ms: Date.parse('2026-08-18T06:14:20Z'),
		duration_ms: 152_000
	};
</script>

<KeepExportPanel
	sourceId="back-yard"
	sourceName="Back Yard"
	{segment}
	bitrateKbps={6_200}
	jobPresentation
/>
