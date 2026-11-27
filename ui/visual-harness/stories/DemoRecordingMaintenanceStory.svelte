<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import { setCapabilityState } from '$lib/capability-context';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import RecordingReconciliation from '$lib/components/RecordingReconciliation.svelte';
	import type { ControlClient } from '$lib/control-client';
	import {
		RECORDING_MAINTENANCE_CAPABILITY,
		RecordingMaintenanceClient
	} from '$lib/control-client-maintenance';
	import { setControlClient } from '$lib/control-context';
	import {
		RecordingDeletionJobListSchema,
		RecordingDeletionJobSchema,
		RecordingDeletionReason,
		RecordingDeletionStatus,
		RecordingDriftKind,
		RecordingReconciliationReportSchema,
		RecordingRemedy,
		type Ok,
		type RecordingDeletionJob,
		type RecordingReconciliationReport,
		type Request
	} from '$lib/proto/webrtc_pb';
	import type { CameraListItem } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import TrashIcon from '@lucide/svelte/icons/trash-2';
	import XIcon from '@lucide/svelte/icons/x';

	setCapabilityState([RECORDING_MAINTENANCE_CAPABILITY]);

	const cameras: Pick<CameraListItem, 'id' | 'name'>[] = [
		{ id: 'front-door', name: 'Front Door' },
		{ id: 'porch', name: 'Porch' },
		{ id: 'driveway', name: 'Driveway' }
	];

	const priorJob = create(RecordingDeletionJobSchema, {
		jobId: 'job-20260817-a1b2c3d4e5f6',
		reason: RecordingDeletionReason.OPERATOR,
		revision: 39n,
		status: RecordingDeletionStatus.DELETED,
		bytes: 67_108_864n,
		createdAtMs: BigInt(Date.parse('2026-08-17T22:15:00Z')),
		expiresAtMs: BigInt(Date.parse('2026-08-17T22:25:00Z')),
		requiredConfirmationText: 'DELETE 1',
		deletedCount: 1,
		failedCount: 0,
		objects: [
			{
				recordingId: 'rec-porch-sub-2200',
				startMs: BigInt(Date.parse('2026-08-17T22:00:00Z')),
				endMs: BigInt(Date.parse('2026-08-17T22:15:00Z')),
				bytes: 67_108_864n,
				status: RecordingDeletionStatus.DELETED
			}
		]
	});

	function buildDeletionJob(
		status: RecordingDeletionStatus,
		includeNonce = false
	): RecordingDeletionJob {
		const deletedCount = status === RecordingDeletionStatus.DELETED ? 2 : 0;
		return create(RecordingDeletionJobSchema, {
			jobId: 'job-20260818-9f0a44c2e110',
			reason: RecordingDeletionReason.OPERATOR,
			revision: 41n,
			status,
			bytes: 195_454_566n,
			createdAtMs: BigInt(Date.parse('2026-08-18T06:35:00Z')),
			expiresAtMs: BigInt(Date.parse('2026-08-18T06:45:00Z')),
			confirmationNonce: includeNonce ? 'a'.repeat(64) : undefined,
			requiredConfirmationText: 'DELETE 2',
			gaps: [],
			bookmarkEventIds: ['evt-bm-01'],
			relatedExportIds: ['exp-clip-09'],
			consequences: [
				'Whole recordings overlapping the selected range are permanently removed without trimming boundaries.',
				'Bookmarks do not protect footage from deletion, and ready exports remain untouched.'
			],
			deletedCount,
			failedCount: 0,
			objects: [
				{
					recordingId: 'rec-front-door-main-0500',
					startMs: BigInt(Date.parse('2026-08-18T05:00:00Z')),
					endMs: BigInt(Date.parse('2026-08-18T05:30:00Z')),
					bytes: 98_775_859n,
					status
				},
				{
					recordingId: 'rec-front-door-main-0530',
					startMs: BigInt(Date.parse('2026-08-18T05:30:00Z')),
					endMs: BigInt(Date.parse('2026-08-18T06:00:00Z')),
					bytes: 96_678_707n,
					status
				}
			]
		});
	}

	let reconciliationReport: RecordingReconciliationReport = create(
		RecordingReconciliationReportSchema,
		{
			reportId: 'rec-report-20260818-01',
			revision: 42n,
			complete: true,
			inspected: 128,
			items: [
				{
					itemId: 'drift-unknown-1',
					label: '2026-08-18/orphaned-clip.mp4',
					kind: RecordingDriftKind.UNKNOWN_FILE,
					remedies: [RecordingRemedy.IGNORE]
				},
				{
					itemId: 'drift-missing-1',
					recordingId: 'rec-porch-sub-0412',
					label: 'rec-porch-sub-0412',
					kind: RecordingDriftKind.MISSING_FILE,
					remedies: [RecordingRemedy.IGNORE, RecordingRemedy.RETAIN_TOMBSTONE]
				},
				{
					itemId: 'drift-index-1',
					recordingId: 'rec-front-door-main-0615',
					label: 'rec-front-door-main-0615',
					kind: RecordingDriftKind.INDEX_MISMATCH,
					remedies: [RecordingRemedy.IGNORE, RecordingRemedy.REINDEX]
				}
			]
		}
	);

	let deletionStageIndex = 0;
	let remedyApplied = $state(false);

	async function sendMaintenanceCommand(command: Request['command']): Promise<Ok['result']> {
		if (command.case !== 'recordingMaintenanceCommand') {
			throw new Error(`Unexpected command: ${command.case}`);
		}
		const action = command.value.action;
		if (action.case === 'list') {
			return {
				case: 'recordingDeletionJobs',
				value: create(RecordingDeletionJobListSchema, { jobs: [priorJob], nextJobId: '' })
			};
		}
		if (action.case === 'preview') {
			deletionStageIndex = 0;
			return {
				case: 'recordingDeletionJob',
				value: buildDeletionJob(RecordingDeletionStatus.PREPARED, true)
			};
		}
		if (action.case === 'confirm') {
			deletionStageIndex = 0;
			return {
				case: 'recordingDeletionJob',
				value: buildDeletionJob(RecordingDeletionStatus.QUEUED)
			};
		}
		if (action.case === 'get') {
			deletionStageIndex = Math.min(2, deletionStageIndex + 1);
			const nextStatus =
				deletionStageIndex === 1 ? RecordingDeletionStatus.STAGED : RecordingDeletionStatus.DELETED;
			return {
				case: 'recordingDeletionJob',
				value: buildDeletionJob(nextStatus)
			};
		}
		if (action.case === 'inspectCatalog') {
			return { case: 'recordingReconciliationReport', value: reconciliationReport };
		}
		if (action.case === 'applyRemedy') {
			const { itemId, remedy } = action.value;
			reconciliationReport = create(RecordingReconciliationReportSchema, {
				...reconciliationReport,
				items: reconciliationReport.items.map((item) =>
					item.itemId === itemId ? { ...item, appliedRemedy: remedy } : item
				)
			});
			if (remedy === RecordingRemedy.REINDEX) remedyApplied = true;
			return { case: 'recordingReconciliationReport', value: reconciliationReport };
		}
		return {
			case: 'recordingDeletionJob',
			value: buildDeletionJob(RecordingDeletionStatus.CANCELLED)
		};
	}

	const maintenanceClient = new RecordingMaintenanceClient(sendMaintenanceCommand);
	setControlClient({ recordingMaintenance: maintenanceClient } as unknown as ControlClient);

	let view = $state<'deletion' | 'reconciliation'>('deletion');
	let sourceId = $state('front-door');
	let streamId = $state('sub');
	let reason = $state(RecordingDeletionReason.OPERATOR);
	let start = $state('2026-08-18T05:00');
	let end = $state('2026-08-18T06:00');
	let job = $state.raw<RecordingDeletionJob | null>(null);
	let history = $state.raw<RecordingDeletionJob[]>([priorJob]);
	let busy = $state(false);
	let message = $state('');
	let confirmation = $state('');
	let previewOpen = $state(false);
	let dialog = $state<HTMLDialogElement>();

	let running = $derived(
		job !== null &&
			(job.status === RecordingDeletionStatus.QUEUED ||
				job.status === RecordingDeletionStatus.WORKING ||
				job.status === RecordingDeletionStatus.STAGED)
	);

	let ledgerStep = $derived(
		job === null
			? -1
			: job.status === RecordingDeletionStatus.QUEUED
				? 0
				: job.status === RecordingDeletionStatus.STAGED
					? 1
					: job.status === RecordingDeletionStatus.DELETED
						? 2
						: -1
	);

	$effect(() => {
		if (!dialog) return;
		if (previewOpen && !dialog.open) dialog.showModal();
		else if (!previewOpen && dialog.open) dialog.close();
	});

	$effect(() => {
		const current = job;
		if (!current || !running) return;
		let active = true;
		const timer = setTimeout(() => {
			void maintenanceClient.get(current.jobId).then((updated) => {
				if (!active) return;
				job = updated;
				if (updated.status === RecordingDeletionStatus.DELETED) {
					history = [updated, priorJob];
				}
			});
		}, 1_000);
		return () => {
			active = false;
			clearTimeout(timer);
		};
	});

	async function previewDeletion(): Promise<void> {
		if (busy) return;
		busy = true;
		message = '';
		confirmation = '';
		try {
			job = await maintenanceClient.preview(
				{ sourceId, streamId, startMs: Date.parse(`${start}Z`), endMs: Date.parse(`${end}Z`) },
				reason
			);
			previewOpen = job.status === RecordingDeletionStatus.PREPARED;
		} catch (cause) {
			message = cause instanceof Error ? cause.message : 'Preview unavailable.';
		} finally {
			busy = false;
		}
	}

	async function confirmDeletion(): Promise<void> {
		const selected = job;
		if (!selected || busy) return;
		busy = true;
		message = '';
		try {
			job = await maintenanceClient.confirm(selected, confirmation);
			confirmation = '';
			previewOpen = false;
		} catch (cause) {
			message = cause instanceof Error ? cause.message : 'Confirmation failed.';
		} finally {
			busy = false;
		}
	}

	function dismissPreview(): void {
		maintenanceClient.discardConfirmations();
		confirmation = '';
		previewOpen = false;
	}

	function formatTimestamp(value: bigint | undefined): string {
		return value === undefined
			? 'Unknown end'
			: new Date(Number(value)).toISOString().replace('T', ' ').replace('.000Z', ' UTC');
	}

	function formatMiB(value: bigint): string {
		return `${(Number(value) / 1_048_576).toFixed(2)} MiB`;
	}

	function formatStatus(value: RecordingDeletionStatus): string {
		return (RecordingDeletionStatus[value] ?? 'UNKNOWN').toLowerCase().replaceAll('_', ' ');
	}
</script>

<main
	data-paper-scenario="settings.desktop.storage-retention"
	data-demo-recording-maintenance
	data-demo-maintenance-complete={remedyApplied ? 'true' : undefined}
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="keep" />

	<div class="flex min-w-0 flex-1 flex-col overflow-hidden">
		<header
			class="flex h-[56px] shrink-0 items-center justify-between border-b border-hairline bg-surface px-6"
		>
			<div class="flex items-baseline gap-3">
				<h1 class="text-base font-semibold">Recording maintenance</h1>
				<span class="font-mono text-2xs tracking-caps text-text-faint">
					WHOLE-RECORDING DELETION &amp; BOUNDED RECONCILIATION
				</span>
			</div>
			<span class="font-mono text-2xs text-primary-soft">KEEPPEEK.RECORDING-MAINTENANCE.V1</span>
		</header>

		<div class="min-h-0 flex-1 overflow-hidden px-6 py-4">
			<div class="mx-auto flex h-full max-w-5xl flex-col gap-5">
				<nav aria-label="Maintenance views" class="flex gap-1 border-b border-hairline text-sm">
					<button
						type="button"
						data-demo-action="tab-deletion"
						class="border-b-2 px-4 py-2.5 font-medium {view === 'deletion'
							? 'border-primary text-foreground'
							: 'border-transparent text-text-muted'}"
						aria-pressed={view === 'deletion'}
						onclick={() => (view = 'deletion')}>Recordings</button
					>
					<button
						type="button"
						data-demo-action="tab-reconciliation"
						class="border-b-2 px-4 py-2.5 font-medium {view === 'reconciliation'
							? 'border-primary text-foreground'
							: 'border-transparent text-text-muted'}"
						aria-pressed={view === 'reconciliation'}
						onclick={() => (view = 'reconciliation')}>Reconciliation</button
					>
				</nav>

				{#if view === 'reconciliation'}
					<RecordingReconciliation enabled={true} />
				{:else}
					<form
						class="grid grid-cols-3 gap-3 border-b border-hairline pb-4"
						onsubmit={(event) => {
							event.preventDefault();
							void previewDeletion();
						}}
					>
						<label class="grid gap-1 text-xs text-text-muted"
							>Camera<select
								data-demo-action="select-camera"
								class="h-10 min-w-0 rounded-sm border border-hairline-strong bg-surface px-2 text-sm text-foreground"
								bind:value={sourceId}
								disabled={busy}
							>
								{#each cameras as camera (camera.id)}
									<option value={camera.id}>{camera.name || camera.id}</option>
								{/each}
							</select></label
						>
						<label class="grid gap-1 text-xs text-text-muted"
							>Stream<select
								data-demo-action="select-stream"
								class="h-10 rounded-sm border border-hairline-strong bg-surface px-2 text-sm text-foreground"
								bind:value={streamId}
								disabled={busy}
							>
								<option value="sub">Sub</option>
								<option value="main">Main</option>
							</select></label
						>
						<label class="grid gap-1 text-xs text-text-muted"
							>Reason<select
								class="h-10 rounded-sm border border-hairline-strong bg-surface px-2 text-sm text-foreground"
								bind:value={reason}
								disabled={busy}
							>
								<option value={RecordingDeletionReason.OPERATOR}>Operator cleanup</option>
								<option value={RecordingDeletionReason.PRIVACY}>Privacy request</option>
							</select></label
						>
						<label class="grid gap-1 text-xs text-text-muted"
							>Start (UTC)<input
								class="h-10 min-w-0 rounded-sm border border-hairline-strong bg-surface px-2 font-mono text-xs text-foreground"
								type="datetime-local"
								required
								bind:value={start}
								disabled={busy}
							/></label
						>
						<label class="grid gap-1 text-xs text-text-muted"
							>End (UTC)<input
								class="h-10 min-w-0 rounded-sm border border-hairline-strong bg-surface px-2 font-mono text-xs text-foreground"
								type="datetime-local"
								required
								bind:value={end}
								disabled={busy}
							/></label
						>
						<button
							type="submit"
							data-demo-action="preview-deletion"
							class="flex h-10 items-center justify-center gap-2 self-end rounded-sm border border-destructive bg-destructive/10 px-4 text-sm font-medium text-destructive disabled:opacity-40"
							disabled={busy || !sourceId}
						>
							<TrashIcon class="size-4" />Preview deletion
						</button>
					</form>

					{#if message}
						<p role="alert" class="text-sm text-destructive">{message}</p>
					{/if}

					{#if job && !previewOpen}
						<section
							aria-label="Deletion progress"
							class="space-y-3 rounded-md border border-hairline bg-surface p-4"
						>
							<div class="flex flex-wrap items-center gap-3">
								<h2 class="text-base font-semibold capitalize">{formatStatus(job.status)}</h2>
								<span class="font-mono text-xs text-text-muted">
									{job.deletedCount} deleted · {job.failedCount} failed · {formatMiB(job.bytes)}
								</span>
								<div class="flex-1"></div>
								<div class="flex items-center gap-1.5 font-mono text-2xs">
									{#each ['Queued', 'Staged', 'Deleted'] as stage, index (stage)}
										<span
											class="inline-flex items-center gap-1 rounded-sm border px-2 py-1 {index <
											ledgerStep
												? 'border-healthy/40 bg-healthy/10 text-healthy'
												: index === ledgerStep
													? 'border-primary bg-primary/15 text-primary-soft'
													: 'border-hairline text-text-faint'}"
										>
											{#if index <= ledgerStep && ledgerStep === 2}
												<CheckIcon class="size-3" />
											{/if}
											{stage}
										</span>
									{/each}
								</div>
							</div>
							{@render objectList(job)}
						</section>
					{/if}

					<section aria-label="Maintenance history" class="space-y-2 border-t border-hairline pt-4">
						<h2 class="text-sm font-semibold">History</h2>
						<ul class="divide-y divide-hairline">
							{#each history as entry (entry.jobId)}
								<li class="flex items-center gap-5 py-2.5 text-xs">
									<span class="font-mono text-text-muted">{entry.jobId.slice(0, 16)}</span>
									<span class="font-medium capitalize">{formatStatus(entry.status)}</span>
									<span class="text-text-muted">{formatTimestamp(entry.createdAtMs)}</span>
									<span class="ml-auto font-mono">{formatMiB(entry.bytes)}</span>
								</li>
							{/each}
						</ul>
					</section>
				{/if}
			</div>
		</div>
	</div>

	<dialog
		bind:this={dialog}
		oncancel={dismissPreview}
		class="m-auto max-h-[720px] w-[760px] overflow-y-auto rounded-md border border-hairline-strong bg-ground p-0 text-foreground shadow-2xl backdrop:bg-black/60"
		aria-labelledby="delete-title"
	>
		{#if job}
			<div class="flex items-center gap-3 border-b border-hairline p-4">
				<TrashIcon class="size-5 shrink-0 text-destructive" />
				<h2 id="delete-title" class="text-base font-semibold">
					Delete {job.objects.length}
					{job.objects.length === 1 ? 'recording' : 'recordings'} permanently?
				</h2>
				<button
					class="ml-auto grid size-9 shrink-0 place-items-center"
					type="button"
					onclick={dismissPreview}
					aria-label="Close preview"
				>
					<XIcon class="size-4" />
				</button>
			</div>
			<div class="space-y-4 p-4">
				<p class="text-sm">
					{formatMiB(job.bytes)} · whole-recording boundaries · catalog revision {job.revision.toString()}
				</p>
				{@render objectList(job)}
				<dl class="grid grid-cols-3 gap-3 rounded-sm border border-hairline bg-surface p-3 text-xs">
					<div>
						<dt class="text-text-muted">Gaps</dt>
						<dd class="mt-0.5 font-mono text-sm">{job.gaps.length}</dd>
					</div>
					<div>
						<dt class="text-text-muted">Bookmarks</dt>
						<dd class="mt-0.5 font-mono text-sm">{job.bookmarkEventIds.length}</dd>
					</div>
					<div>
						<dt class="text-text-muted">Related exports</dt>
						<dd class="mt-0.5 font-mono text-sm">{job.relatedExportIds.length}</dd>
					</div>
				</dl>
				<ul class="space-y-1 text-xs text-text-muted">
					{#each job.consequences as consequence (consequence)}
						<li>{consequence}</li>
					{/each}
				</ul>
				<label class="grid gap-1.5 text-sm"
					>Type <span class="font-mono font-semibold">{job.requiredConfirmationText}</span><input
						data-demo-action="confirm-delete-input"
						class="h-10 rounded-sm border border-hairline-strong bg-surface px-3 font-mono text-sm"
						bind:value={confirmation}
						disabled={busy}
						autocomplete="off"
						spellcheck="false"
					/></label
				>
				<div class="flex justify-end gap-3">
					<button
						class="h-10 rounded-sm border border-hairline px-4 text-sm"
						type="button"
						onclick={dismissPreview}>Keep recordings</button
					>
					<button
						data-demo-action="confirm-delete"
						class="text-destructive-foreground flex h-10 items-center justify-center gap-2 rounded-sm bg-destructive px-4 text-sm font-medium disabled:opacity-40"
						type="button"
						disabled={busy || confirmation !== job.requiredConfirmationText}
						onclick={() => void confirmDeletion()}
					>
						<TrashIcon class="size-4" />Delete permanently
					</button>
				</div>
			</div>
		{/if}
	</dialog>
</main>

{#snippet objectList(value: RecordingDeletionJob)}
	<ul aria-label="Recording objects" class="divide-y divide-hairline border-y border-hairline">
		{#each value.objects as object (object.recordingId)}
			<li class="grid grid-cols-[minmax(0,1fr)_2fr_auto_auto] items-center gap-3 py-2.5 text-xs">
				<span class="font-mono break-all text-text-muted">{object.recordingId}</span>
				<span class="font-mono text-2xs"
					>{formatTimestamp(object.startMs)} → {formatTimestamp(object.endMs)}</span
				>
				<span class="font-mono">{formatMiB(object.bytes)}</span>
				<span class="font-mono capitalize">{formatStatus(object.status)}</span>
			</li>
		{/each}
	</ul>
{/snippet}
