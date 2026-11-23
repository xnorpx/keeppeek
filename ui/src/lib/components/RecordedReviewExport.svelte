<script lang="ts">
	import { useControlClient } from '$lib/control-context';
	import { reviewExportRecordings } from '$lib/recorded-review-requests';
	import { segmentForExportRange } from '$lib/keep-modes';
	import type { KeepReviewRange } from '$lib/keep-link';
	import type { CameraListItem, RecordingSegment } from '$lib/types';
	import KeepExportPanel from './KeepExportPanel.svelte';

	let {
		camera,
		stream,
		range,
		onback
	}: {
		camera: CameraListItem;
		stream: RecordingSegment['stream'];
		range: KeepReviewRange;
		onback: () => void;
	} = $props();
	const client = useControlClient();
	let segment = $state.raw<RecordingSegment | null>(null);
	let problem = $state<string | null>(null);
	let retry = $state(0);

	$effect(() => {
		const source = camera.id,
			selectedStream = stream,
			{ startMs, endMs } = range;
		const controller = new AbortController();

		void retry;
		segment = null;
		problem = null;
		async function load() {
			const rows = await reviewExportRecordings(
				client,
				source,
				{ startMs, endMs },
				controller.signal
			);
			if (controller.signal.aborted) return;
			segment = segmentForExportRange(rows, selectedStream, startMs, endMs);
			if (!segment)
				problem =
					'The focused stream no longer covers the entire selected range. Return to the comparison to adjust it.';
		}
		void load().catch(() => {
			if (!controller.signal.aborted) problem = 'Recording coverage could not be loaded.';
		});
		return () => controller.abort();
	});
</script>

<section class="mx-auto flex h-full max-w-3xl flex-col gap-4 overflow-auto p-4">
	<header>
		<button type="button" class="min-h-11 underline" onclick={onback}>Back to comparison</button>
		<h1 class="text-xl font-semibold">Export focused camera</h1>
		<p>{camera.name || camera.id} · {stream}</p>
	</header>
	{#if problem}
		<p role="alert">{problem}</p>
		<button type="button" class="min-h-11 self-start px-3" onclick={() => retry++}
			>Recheck coverage</button
		>
	{:else if segment}
		<KeepExportPanel
			sourceId={camera.id}
			sourceName={camera.name || camera.id}
			{segment}
			bitrateKbps={camera.profiles.find((profile) => profile.stream === stream)?.bitrate_kbps ??
				null}
			rangeStartMs={range.startMs}
			rangeEndMs={range.endMs}
		/>
	{:else}<p role="status">Checking recorded coverage…</p>{/if}
</section>
