<script lang="ts">
	import { onDestroy } from 'svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import {
		ReportSubmission,
		reportTextFields,
		type ReportText,
		type ReportTransport,
		type ReportProgress
	} from '$lib/report-submission';
	import { diagnosticsBundleFilename, downloadDiagnosticsArtifact } from '$lib/diagnostics-bundle';
	import { reportCaptureSupported } from '$lib/report-capture';
	import type { ReportSession } from '$lib/report-session.svelte';
	import type { ReportingAuditPayload } from '$lib/types';

	let {
		session,
		open = $bindable(false),
		reporting,
		onaudit
	}: {
		session: ReportSession;
		open?: boolean;
		reporting?: { transport: ReportTransport; retentionDays: number };
		onaudit?: (payload: ReportingAuditPayload) => void;
	} = $props();

	let dialog: HTMLDialogElement | undefined = $state();
	let preview: string | null = $state(null);
	let elapsed = $state(0);
	let review = $derived(session.review);

	const blankDraft = (): ReportText => ({
		title: '',
		description: '',
		reproduction: '',
		expected: '',
		actual: ''
	});
	let draft: ReportText = $state(blankDraft());
	let includeRecording = $state(true);
	let hasDraft = $derived(reportTextFields.some(({ key }) => draft[key].trim()));

	let submission: ReportSubmission | null = $state.raw(null);
	let submissionState = $state<
		'review' | 'confirm' | 'sending' | 'cancelling' | 'retry' | 'complete'
	>('review');
	let checkOnly = $state(false);
	let submissionError: string | null = $state(null);
	let issueUrl: string | null = $state(null);
	let progress: ReportProgress = $state({ uploaded: 0, total: 0 });

	const labels: Record<string, string> = {
		'browser-log-window-incomplete': 'Some browser log entries are missing.',
		'server-log-window-incomplete': 'Some server log entries are missing.',
		'server-log-filter-active': 'The active server log filter may have excluded entries.',
		'server-log-stream-gap': 'The server log stream reported skipped entries.',
		'server-log-generation-changed': 'The recorder restarted during the report.',
		'server-log-generation-unverified':
			'The recorder history could not be verified. Server logs were excluded.',
		'server-logs-unavailable': 'Server logs could not be collected.',
		'diagnostics-logging-settings-unavailable': 'Recorder logging settings could not be collected.',
		'diagnostics-health-unavailable': 'Recorder health could not be collected.',
		'diagnostics-runtime-config-unavailable':
			'Recorder runtime configuration could not be collected.',
		'diagnostics-cameras-unavailable': 'Camera inventory could not be collected.',
		'diagnostics-metrics-unavailable': 'Recorder metrics could not be collected.',
		'diagnostics-package-unavailable': 'The diagnostics package could not be prepared.'
	};

	$effect(() => {
		if (submissionState === 'confirm')
			dialog?.querySelector<HTMLButtonElement>('[data-confirm-report]')?.focus();
	});

	$effect(() => {
		if (!review) resetSubmission();
	});
	onDestroy(resetSubmission);

	function downloadDraft(stopUtc: string): void {
		const lines = reportTextFields
			.filter(({ key }) => draft[key].trim())
			.map(({ key, label }) => `${label}\n${draft[key].trim()}`);
		const name = new Date(stopUtc).toISOString().replace(/[:.]/g, '-');
		downloadDiagnosticsArtifact(
			new Blob([lines.join('\n\n')], { type: 'text/plain;charset=utf-8' }),
			`keeppeek-report-notes-${name}.txt`
		);
		onaudit?.({ event: 'local_download' });
	}

	function downloadRecord(blob: Blob, stopUtc: string): void {
		const extension = blob.type === 'video/mp4' ? 'mp4' : 'webm';
		downloadDiagnosticsArtifact(
			blob,
			`keeppeek-screen-${new Date(stopUtc).toISOString().replace(/[:.]/g, '-')}.${extension}`
		);
		onaudit?.({ event: 'local_download' });
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

	function prepareSubmission(): void {
		if (!reporting || !review?.diagnostics || !review.redactText || submission) return;
		try {
			submission = new ReportSubmission(reporting.transport, {
				text: draft,
				redactText: review.redactText,
				diagnostics: review.diagnostics,
				screen: includeRecording ? (review.recording ?? undefined) : undefined,
				evidenceWarnings: review.evidenceWarnings
			});
			submissionError = null;
			submissionState = 'confirm';
		} catch (error) {
			submissionError =
				error instanceof Error ? error.message : 'Review the report before submitting.';
		}
	}

	async function submitConfirmed(): Promise<void> {
		const attempt = submission;
		if (!attempt || !['confirm', 'retry'].includes(submissionState)) return;
		submissionState = 'sending';
		submissionError = null;
		try {
			const result = await (checkOnly
				? attempt.check()
				: attempt.submit((next) => {
						progress = next;
					}));
			if (submission !== attempt || submissionState !== 'sending') return;
			if (checkOnly && result.state === 'cancelled') {
				resetSubmission();
				return;
			}
			if (result.state === 'complete') {
				let url: URL | undefined;
				try {
					url = new URL(result.issueUrl);
				} catch {
					// The completed submission remains valid even if its link is unusable.
				}
				if (
					!url ||
					url.origin !== 'https://github.com' ||
					url.username ||
					url.password ||
					url.search ||
					url.hash ||
					!/^\/[^/]+\/[^/]+\/issues\/[1-9][0-9]*$/.test(url.pathname)
				) {
					submissionState = 'complete';
					session.markCompleted();
					submissionError = 'The service created the issue but returned an invalid issue link.';
					onaudit?.({ event: 'complete', report_id: attempt.reportId });
					return;
				}
				issueUrl = url.href;
				submissionState = 'complete';
				session.markCompleted();
				onaudit?.({ event: 'complete', report_id: attempt.reportId });
				return;
			}
			onaudit?.({ event: 'failed', report_id: attempt.reportId });
			submissionError =
				'The service has not returned an issue. You can check again or download the report.';
		} catch {
			if (submission !== attempt || submissionState !== 'sending') return;
			onaudit?.({ event: 'failed', report_id: attempt.reportId });
			submissionError =
				'The service could not confirm submission. You can retry or download the report.';
		}
		submissionState = 'retry';
	}

	async function cancelUpload(): Promise<void> {
		const attempt = submission;
		if (!attempt || submissionState !== 'sending') return;
		submissionState = 'cancelling';
		const confirmed = await attempt.cancel();
		if (submission !== attempt) return;
		if (confirmed) {
			resetSubmission();
			return;
		}
		checkOnly = true;
		submissionState = 'retry';
		submissionError =
			'The service could not confirm cancellation. Check the existing submission before trying again.';
	}

	function resetSubmission(): void {
		void submission?.cancel();
		submission = null;
		submissionState = 'review';
		submissionError = null;
		issueUrl = null;
		checkOnly = false;
		progress = { uploaded: 0, total: 0 };
	}

	function start(display: boolean) {
		draft = blankDraft();
		includeRecording = true;
		open = false;
		session.start(display);
	}

	function discard() {
		resetSubmission();
		draft = blankDraft();
		includeRecording = true;
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
		if (!review) open = false;
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
			{#if review.evidenceWarnings.length}
				<ul class="list-disc space-y-1 pl-5 text-sm">
					{#each review.evidenceWarnings as warning (warning)}
						<li>{labels[warning] ?? 'Some evidence could not be collected.'}</li>
					{/each}
				</ul>
			{/if}
			{#if submission}
				<section aria-label="Reviewed issue text" class="space-y-2">
					<h3 class="text-sm font-semibold">Issue text to be shared</h3>
					<dl class="space-y-2 text-sm">
						{#each reportTextFields as field (field.key)}
							<div>
								<dt class="font-medium">{field.label}</dt>
								<dd class="wrap-anywhere whitespace-pre-wrap"
									>{submission.reviewedText[field.key]}</dd
								>
							</div>
						{/each}
					</dl>
				</section>
			{:else}
				<fieldset class="space-y-3">
					<legend class="mb-2 text-sm font-semibold">Issue details</legend>
					<p class="text-sm text-muted-foreground"
						>Remove private details from the issue text before sharing.</p
					>
					{#each reportTextFields as field (field.key)}
						<label class="block space-y-1 text-sm">
							<span>{field.label}</span>
							{#if field.key === 'title'}
								<Input bind:value={draft.title} maxlength={field.limit} />
							{:else}
								<textarea
									bind:value={draft[field.key]}
									maxlength={field.limit}
									rows={2}
									class="block min-h-16 w-full resize-y rounded-md border border-input bg-background px-3 py-2"
								></textarea>
							{/if}
						</label>
					{/each}
				</fieldset>
			{/if}
			<ul aria-label="Report artifacts" class="space-y-1 text-sm">
				{#if review.diagnostics}
					<li>
						Diagnostics · {(review.diagnostics.size / 1024).toFixed(1)} KiB
						<span class="block text-muted-foreground"
							>{review.browserLogCount} browser and {review.serverLogCount} server log entries</span
						>
					</li>
				{/if}
				{#if review.recording && includeRecording}
					<li>Private screen recording · {(review.recording.size / 1_048_576).toFixed(1)} MiB</li>
				{/if}
			</ul>
			{#if review.recording}
				<label class="flex items-center gap-2 text-sm">
					<input
						type="checkbox"
						bind:checked={includeRecording}
						disabled={submissionState !== 'review'}
					/>
					Include the screen recording
				</label>
			{/if}
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
			{#if !reporting}
				<p class="text-sm text-muted-foreground"
					>Remote submission is unavailable. Download the artifacts you want to share.</p
				>
			{:else if submissionState === 'confirm'}
				<p class="text-sm"
					>Submit the issue text and listed artifacts. The service keeps private artifacts for up to {reporting.retentionDays}
					days.</p
				>
				<div class="flex flex-wrap gap-2">
					<Button data-confirm-report onclick={() => void submitConfirmed()}
						>Confirm and submit</Button
					>
					<Button variant="outline" onclick={resetSubmission}>Back to review</Button>
				</div>
			{:else if submissionState === 'sending'}
				<p class="text-sm" role="status"
					>{progress.uploaded === progress.total && progress.total > 0
						? 'Waiting for issue creation'
						: 'Uploading reviewed artifacts'}</p
				>
				{#if progress.total > 0}
					<progress
						aria-label="Report upload progress"
						value={progress.uploaded}
						max={progress.total}
						class="w-full"
					></progress>
				{/if}
				<Button variant="outline" onclick={() => void cancelUpload()}>Cancel upload</Button>
			{:else if submissionState === 'cancelling'}
				<p role="status" class="text-sm">Checking cancellation</p>
			{:else if submissionState === 'retry'}
				<Button onclick={() => void submitConfirmed()}
					>{checkOnly ? 'Check submission' : 'Retry submission'}</Button
				>
			{:else if submissionState === 'complete'}
				{#if issueUrl}<a href={issueUrl} class="text-sm font-medium underline"
						>Open the created GitHub issue</a
					>{/if}
			{:else if review.diagnostics}
				<Button onclick={prepareSubmission}>Review submission</Button>
			{/if}
			{#if submissionError}<p role="alert" class="text-sm">{submissionError}</p>{/if}

			<div class="flex flex-wrap gap-2">
				{#if hasDraft}
					<Button
						variant="outline"
						onclick={() => {
							if (review) downloadDraft(review.window.stopUtc);
						}}>Download report notes</Button
					>
				{/if}
				{#if review.diagnostics}
					<Button
						variant="outline"
						onclick={() => {
							if (review?.diagnostics) {
								downloadDiagnosticsArtifact(
									review.diagnostics,
									diagnosticsBundleFilename(new Date(review.window.stopUtc))
								);
								onaudit?.({ event: 'local_download' });
							}
						}}>Download report diagnostics</Button
					>
				{/if}
				{#if review.recording && includeRecording}
					<Button
						variant="outline"
						onclick={() => {
							if (review?.recording) downloadRecord(review.recording, review.window.stopUtc);
						}}>Download screen recording</Button
					>
				{/if}
				{#if review.recording}
					<Button
						variant="ghost"
						onclick={() => session.discardRecording()}
						disabled={submissionState !== 'review'}>Discard screen recording</Button
					>
				{/if}
				{#if submissionState === 'retry'}
					<Button variant="outline" onclick={() => (open = false)}>Keep report</Button>
				{:else}
					<Button onclick={discard} disabled={['sending', 'cancelling'].includes(submissionState)}
						>Done</Button
					>
				{/if}
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
