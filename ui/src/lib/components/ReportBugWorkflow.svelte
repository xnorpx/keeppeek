<script lang="ts">
	import { Button } from '$lib/components/ui/button/index.js';
	import { diagnosticsBundleFilename, downloadDiagnosticsArtifact } from '$lib/diagnostics-bundle';
	import { reportCaptureSupported } from '$lib/report-capture';
	import type { ReportSession } from '$lib/report-session.svelte';

	let { session, open = $bindable(false) }: { session: ReportSession; open?: boolean } = $props();
	let dialog: HTMLDialogElement | undefined = $state();
	let preview: string | null = $state(null);
	let elapsed = $state(0);
	let review = $derived(session.review);

	const labels = {
		'browser-log-window-incomplete': 'Some browser log entries are missing.',
		'server-log-window-incomplete': 'Some server log entries are missing.',
		'server-log-filter-active': 'The active server log filter may have excluded entries.',
		'server-log-stream-gap': 'The server log stream reported skipped entries.',
		'server-log-generation-changed': 'The recorder restarted during the report.',
		'server-log-generation-unverified':
			'The recorder history could not be verified. Server logs were excluded.',
		'server-logs-unavailable': 'Server logs could not be collected.'
	};

	function downloadRecord(blob: Blob, stopUtc: string): void {
		const extension = blob.type === 'video/mp4' ? 'mp4' : 'webm';
		downloadDiagnosticsArtifact(
			blob,
			`keeppeek-screen-${new Date(stopUtc).toISOString().replace(/[:.]/g, '-')}.${extension}`
		);
	}

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) dialog.showModal();
		else if (!open && dialog.open) dialog.close();
	});

	$effect(() => {
		if (session.phase === 'review') open = true;
	});

	$effect(() => {
		if (session.phase !== 'recording') return;
		const start = session.startedAt;
		const update = () => {
			elapsed = Math.max(0, Math.floor((Date.now() - start) / 1_000));
		};
		update();
		const timer = setInterval(update, 1_000);
		return () => clearInterval(timer);
	});

	$effect(() => {
		if (!open || !review?.recording) {
			preview = null;
			return;
		}
		const url = URL.createObjectURL(review.recording);
		preview = url;
		return () => URL.revokeObjectURL(url);
	});

	function start(display: boolean) {
		open = false;
		session.start(display);
	}

	function discard() {
		open = false;
		session.cancel();
	}
</script>

{#if session.phase === 'recording' || session.phase === 'finishing'}
	<aside
		aria-label="Bug report in progress"
		class="fixed right-4 bottom-20 z-40 flex max-w-[calc(100%-2rem)] flex-wrap items-center gap-3 rounded-md border border-hairline bg-surface px-3 py-2 text-foreground shadow-md md:bottom-4"
	>
		<div class="text-sm">
			<p role="status">
				{#if session.phase === 'finishing'}Preparing report
				{:else if session.mediaState === 'requesting'}Waiting for screen selection
				{:else if session.mediaState === 'capturing'}Recording screen
				{:else if session.mediaState === 'unavailable'}Screen unavailable. Collecting logs.
				{:else}Collecting diagnostic logs{/if}
			</p>
			<p class="text-xs text-muted-foreground tabular-nums">
				<span class="sr-only">Elapsed time and screen recording size</span>
				{elapsed}s · {(session.bytes / 1_048_576).toFixed(1)} MiB
			</p>
		</div>
		<Button
			variant="outline"
			disabled={session.phase === 'finishing'}
			onclick={() => void session.stop()}>Stop</Button
		>
		<Button variant="ghost" onclick={discard}>Cancel</Button>
	</aside>
{/if}

<dialog
	bind:this={dialog}
	aria-labelledby="bug-report-title"
	class="m-auto max-h-[85dvh] w-[calc(100%-2rem)] max-w-xl overflow-auto rounded-md border border-hairline bg-surface p-0 text-foreground shadow-xl backdrop:bg-black/55"
	oncancel={(event) => {
		event.preventDefault();
		open = false;
	}}
>
	<div class="space-y-4 p-4 sm:p-6">
		<h2 id="bug-report-title" class="text-lg font-semibold"
			>{review ? 'Review bug report' : 'Report a bug'}</h2
		>
		{#if review}
			<p class="text-sm text-muted-foreground"
				>Review the diagnostics before sharing. Automated scrubbing can miss private information.
				Screen recordings may contain private images.</p
			>
			<p class="text-sm"
				>{new Date(review.window.startUtc).toLocaleString()} to {new Date(
					review.window.stopUtc
				).toLocaleTimeString()}</p
			>
			{#if review.stopReason === 'duration-limit' || review.stopReason === 'byte-limit'}
				<p role="status" class="text-sm"
					>{review.stopReason === 'duration-limit'
						? 'The five-minute limit ended capture.'
						: 'The 100 MiB limit ended capture.'}</p
				>
			{/if}
			{#if review.window.warnings.length}
				<ul class="list-disc space-y-1 pl-5 text-sm">
					{#each review.window.warnings as warning (warning)}
						<li>{labels[warning]}</li>
					{/each}
				</ul>
			{/if}
			<ul aria-label="Report artifacts" class="space-y-1 text-sm">
				{#if review.diagnostics}
					<li>Diagnostics · {(review.diagnostics.size / 1024).toFixed(1)} KiB</li>
				{/if}
				{#if review.recording}
					<li>Private screen recording · {(review.recording.size / 1_048_576).toFixed(1)} MiB</li>
				{/if}
			</ul>
			{#if preview}
				<!-- svelte-ignore a11y_media_has_caption (the capture includes no audio tracks) -->
				<video
					aria-label="Private screen recording preview"
					src={preview}
					controls
					class="max-h-64 w-full rounded border border-hairline"
				></video>
			{/if}
			{#if !review.diagnostics}
				<p role="alert" class="text-sm"
					>The recorder could not supply the evidence needed to scrub the diagnostics. The package
					is unavailable.</p
				>
			{/if}
			<p class="text-sm text-muted-foreground"
				>Remote submission is unavailable. Download the artifacts you want to share.</p
			>
			<div class="flex flex-wrap gap-2">
				{#if review.diagnostics}
					<Button
						variant="outline"
						onclick={() => {
							if (review?.diagnostics)
								downloadDiagnosticsArtifact(
									review.diagnostics,
									diagnosticsBundleFilename(new Date(review.window.stopUtc))
								);
						}}>Download report diagnostics</Button
					>
				{/if}
				{#if review.recording}
					<Button
						variant="outline"
						onclick={() => {
							if (review?.recording) downloadRecord(review.recording, review.window.stopUtc);
						}}>Download screen recording</Button
					>
					<Button variant="ghost" onclick={() => session.discardRecording()}
						>Discard screen recording</Button
					>
				{/if}
				<Button onclick={discard}>Done</Button>
			</div>
		{:else if session.phase === 'idle'}
			<p class="text-sm"
				>Collect diagnostics while you reproduce the problem. A screen recording is optional.</p
			>
			<p class="text-sm text-muted-foreground"
				>Your chosen display may show private camera footage, notifications, and other apps.
				KeepPeek captures video only.</p
			>
			<div class="flex flex-wrap gap-2">
				<Button onclick={() => start(false)}>Collect logs only</Button>
				<Button variant="outline" disabled={!reportCaptureSupported()} onclick={() => start(true)}
					>Record screen</Button
				>
				<Button
					variant="ghost"
					onclick={() => {
						open = false;
					}}>Cancel</Button
				>
			</div>
		{:else}
			<p class="text-sm">A report is in progress. Use Stop to prepare it for review.</p>
			<Button
				variant="outline"
				onclick={() => {
					open = false;
				}}>Continue reproducing</Button
			>
		{/if}
	</div>
</dialog>
