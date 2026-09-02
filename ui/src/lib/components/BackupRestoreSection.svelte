<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import ArchiveIcon from '@lucide/svelte/icons/archive';
	import CheckCircleIcon from '@lucide/svelte/icons/circle-check';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import ShieldCheckIcon from '@lucide/svelte/icons/shield-check';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import UploadIcon from '@lucide/svelte/icons/upload';
	import type { ControlClient } from '$lib/control-client';
	import {
		ActivateRestoreRequestSchema,
		BackupPathKind,
		BackupSection,
		CreateBackupRequestSchema,
		CreateRestorePlanRequestSchema,
		DeleteBackupRequestSchema,
		RestoreIssueSeverity,
		RestoreState,
		RollbackRestoreRequestSchema,
		type BackupCapabilities,
		type BackupRecord,
		type RestorePlan
	} from '$lib/proto/backup_pb';
	import { Button } from './ui/button/index.js';
	import { Input } from './ui/input/index.js';

	type Props = {
		controller: ControlClient;
		onrestart: () => void | Promise<void>;
	};

	let { controller, onrestart }: Props = $props();
	let administrator = $state(false);
	let supported = $state(false);
	let loaded = $state(false);
	let loading = $state(false);
	let action = $state<string | null>(null);
	let error = $state<string | null>(null);
	let capabilities = $state.raw<BackupCapabilities | null>(null);
	let backups = $state.raw<readonly BackupRecord[]>([]);
	let selected = $state.raw<BackupRecord | null>(null);
	let uploadedFile = $state.raw<File | null>(null);
	let mappings = $state.raw<Record<number, string>>({});
	let plan = $state.raw<RestorePlan | null>(null);
	let confirmed = $state(false);

	$effect(() => {
		const close = controller.onAccessState((state) => {
			administrator = state.session?.role === 'administrator';
			loadWhenAvailable();
		});
		return close;
	});

	$effect(() => {
		const close = controller.onCapabilities((capabilityIds) => {
			supported = capabilityIds.includes('keeppeek.backup.v1');
			if (!supported) {
				loaded = false;
				capabilities = null;
				backups = [];
				selected = null;
				plan = null;
			}
			loadWhenAvailable();
		});
		return close;
	});

	function loadWhenAvailable(): void {
		if (administrator && supported && !loaded && !loading) void load();
	}

	async function load(): Promise<void> {
		loading = true;
		error = null;
		try {
			const [nextCapabilities, records] = await Promise.all([
				controller.getBackupCapabilities(),
				controller.listBackups()
			]);
			capabilities = nextCapabilities;
			backups = records.backups;
			loaded = true;
		} catch (cause) {
			error = message(cause, 'Backup and restore is unavailable.');
		} finally {
			loading = false;
		}
	}

	async function createBackup(): Promise<void> {
		await run('Creating backup', async () => {
			const record = await controller.createBackup(
				create(CreateBackupRequestSchema, {
					clientRequestId: crypto.randomUUID(),
					sections: [],
					expectedArchiveBytes: 0n
				})
			);
			await refresh(record.backupId);
		});
	}

	async function uploadBackup(event: Event): Promise<void> {
		const file = (event.currentTarget as HTMLInputElement).files?.[0] ?? null;
		if (!file) return;
		uploadedFile = file;
		if (capabilities && BigInt(file.size) > capabilities.maximumUploadBytes) {
			error = `The selected file exceeds ${formatBytes(capabilities.maximumUploadBytes)}.`;
			return;
		}
		await run('Validating upload', async () => {
			const record = await controller.uploadBackup(file);
			await refresh(record.backupId);
		});
	}

	async function refresh(backupId?: string): Promise<void> {
		const records = await controller.listBackups();
		backups = records.backups;
		if (backupId) await selectBackup(backupId);
	}

	async function selectBackup(backupId: string): Promise<void> {
		selected = await controller.inspectBackup(backupId);
		plan = null;
		confirmed = false;
		const next: Record<number, string> = {};
		for (const source of selected.manifest?.sourcePaths ?? []) {
			next[source.kind] = capabilities?.targetPaths.find((target) => target.kind === source.kind)?.path ?? '';
		}
		mappings = next;
	}

	async function createPlan(): Promise<void> {
		if (!selected?.manifest || !capabilities) return;
		await run('Checking restore', async () => {
			plan = await controller.createBackupRestorePlan(
				create(CreateRestorePlanRequestSchema, {
					clientRequestId: crypto.randomUUID(),
					backupId: selected!.backupId,
					sections: [],
					pathMappings: selected!.manifest!.sourcePaths.map((source) => ({
						kind: source.kind,
						sourcePath: source.path,
						targetPath: mappings[source.kind] ?? ''
					})),
					expectedTargetRevision: capabilities!.targetRevision
				})
			);
			confirmed = false;
		});
	}

	async function activate(): Promise<void> {
		if (!plan || !confirmed) return;
		await run('Staging restore', async () => {
			await controller.activateBackupRestore(
				create(ActivateRestoreRequestSchema, {
					clientRequestId: crypto.randomUUID(),
					planId: plan!.planId,
					archiveSha256: plan!.archiveSha256,
					confirm: true
				})
			);
			capabilities = await controller.getBackupCapabilities();
		});
	}

	async function rollback(): Promise<void> {
		const restore = capabilities?.activeRestore;
		if (!restore) return;
		await run('Staging rollback', async () => {
			await controller.rollbackBackupRestore(
				create(RollbackRestoreRequestSchema, {
					clientRequestId: crypto.randomUUID(),
					restoreId: restore.restoreId,
					confirm: true
				})
			);
			capabilities = await controller.getBackupCapabilities();
		});
	}

	async function download(record: BackupRecord): Promise<void> {
		await run('Downloading backup', async () => {
			const result = await controller.downloadBackup(record.backupId);
			const url = URL.createObjectURL(result.blob);
			const anchor = document.createElement('a');
			anchor.href = url;
			anchor.download = result.fileName;
			anchor.click();
			setTimeout(() => URL.revokeObjectURL(url), 0);
		});
	}

	async function remove(record: BackupRecord): Promise<void> {
		await run('Deleting backup', async () => {
			await controller.deleteBackup(
				create(DeleteBackupRequestSchema, {
					clientRequestId: crypto.randomUUID(),
					backupId: record.backupId
				})
			);
			if (selected?.backupId === record.backupId) selected = null;
			await refresh();
		});
	}

	async function run(label: string, operation: () => Promise<void>): Promise<void> {
		if (action) return;
		action = label;
		error = null;
		try {
			await operation();
		} catch (cause) {
			error = message(cause, `${label} failed.`);
		} finally {
			action = null;
		}
	}

	function message(cause: unknown, fallback: string): string {
		return cause instanceof Error && cause.message ? cause.message : fallback;
	}

	function formatBytes(bytes: bigint): string {
		if (bytes < 1024n) return `${bytes} B`;
		if (bytes < 1024n * 1024n) return `${Number(bytes / 1024n).toLocaleString()} KiB`;
		return `${Number(bytes / (1024n * 1024n)).toLocaleString()} MiB`;
	}

	function sectionLabel(section: BackupSection): string {
		return BackupSection[section]?.replace('BACKUP_SECTION_', '').replaceAll('_', ' ').toLowerCase() ?? 'unknown';
	}

	function pathLabel(kind: BackupPathKind): string {
		return BackupPathKind[kind]?.replace('BACKUP_PATH_KIND_', '').replaceAll('_', ' ').toLowerCase() ?? 'path';
	}

	function issueClass(severity: RestoreIssueSeverity): string {
		return severity === RestoreIssueSeverity.BLOCKING ? 'text-destructive' : 'text-activity';
	}

	function issueLabel(severity: RestoreIssueSeverity): string {
		return RestoreIssueSeverity[severity]
			.replace('RESTORE_ISSUE_SEVERITY_', '')
			.toLowerCase();
	}
</script>

{#if administrator && supported}
	<section id="backups" class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface" aria-labelledby="backup-heading">
		<header class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5">
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">RECOVERY · PROTOJSON · LOCAL</p>
				<h2 id="backup-heading" class="mt-1 text-xl font-semibold">Backup and restore</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">Create reference-only recovery bundles, inspect every section, and stage an atomic restore before restarting.</p>
			</div>
			<Button variant="outline" size="sm" onclick={() => void load()} disabled={loading || action !== null}>
				<RefreshCwIcon class={loading ? 'animate-spin' : undefined} /> Refresh
			</Button>
		</header>

		{#if error}
			<p class="border-b border-destructive/30 bg-destructive/5 px-5 py-3 text-sm text-destructive" role="alert">{error}</p>
		{/if}
		{#if action}
			<p class="border-b border-hairline px-5 py-2 font-mono text-xs text-text-muted" role="status">{action}…</p>
		{/if}

		<div class="grid lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
			<div class="space-y-5 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div class="flex flex-wrap gap-2">
					<Button onclick={() => void createBackup()} disabled={!loaded || action !== null}><ArchiveIcon /> Create backup</Button>
					<label class="inline-flex h-9 cursor-pointer items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-sm font-medium focus-within:ring-2 focus-within:ring-ring">
						<UploadIcon class="size-4" /> Upload ZIP
						<input class="sr-only" type="file" accept=".zip,application/zip" onchange={uploadBackup} />
					</label>
				</div>
				{#if uploadedFile}
					<p class="text-xs text-text-muted">Selected upload: <span class="font-mono">{uploadedFile.name}</span></p>
				{/if}
				<div>
					<h3 class="text-sm font-semibold">Retained backups</h3>
					{#if backups.length === 0}
						<p class="mt-2 text-sm text-text-faint">No managed backups.</p>
					{:else}
						<ul class="mt-2 divide-y divide-hairline border-y border-hairline">
							{#each backups as backup (backup.backupId)}
								<li class="flex min-w-0 items-center gap-2 py-2.5">
									<button type="button" class="min-w-0 flex-1 text-left focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none" onclick={() => void selectBackup(backup.backupId)}>
										<span class="block truncate text-sm font-medium">{backup.fileName}</span>
										<span class="font-mono text-2xs text-text-faint">{formatBytes(backup.archiveBytes)} · {new Date(Number(backup.createdAtUnixMs)).toISOString()}</span>
									</button>
									<Button variant="ghost" size="icon-sm" aria-label={`Download ${backup.fileName}`} onclick={() => void download(backup)}><DownloadIcon /></Button>
									<Button variant="ghost" size="icon-sm" aria-label={`Delete ${backup.fileName}`} onclick={() => void remove(backup)}><Trash2Icon /></Button>
								</li>
							{/each}
						</ul>
					{/if}
				</div>
				{#if capabilities?.activeRestore}
					<div class="border-t border-hairline pt-4">
						<h3 class="flex items-center gap-2 text-sm font-semibold"><ShieldCheckIcon class="size-4" /> Recovery point</h3>
						<p class="mt-1 text-xs text-text-muted">State: {RestoreState[capabilities.activeRestore.state].replace('RESTORE_STATE_', '').toLowerCase()}</p>
						{#if capabilities.activeRestore.progress}
							<p class="mt-1 font-mono text-2xs text-text-faint">
								Progress {Math.floor(capabilities.activeRestore.progress.completedPerMille / 10)}%
							</p>
						{/if}
						{#if capabilities.activeRestore.healthChecks.length > 0}
							<ul class="mt-3 space-y-2" aria-label="Restore health checks">
								{#each capabilities.activeRestore.healthChecks as check (check.name)}
									<li class="flex items-start gap-2 text-xs text-healthy">
										<CheckCircleIcon class="mt-0.5 size-3.5 shrink-0" />
										<span><span class="font-medium">{check.name.replaceAll('_', ' ')}</span> · {check.detail}</span>
									</li>
								{/each}
							</ul>
						{/if}
						<div class="mt-3 flex flex-wrap gap-2">
							<Button onclick={onrestart}><RotateCcwIcon /> Restart to apply</Button>
							{#if capabilities.activeRestore.state === RestoreState.COMPLETE}
								<Button variant="outline" onclick={() => void rollback()} disabled={action !== null}>Stage rollback</Button>
							{/if}
						</div>
					</div>
				{/if}
			</div>

			<div class="space-y-5 p-5">
				{#if !selected}
					<div class="grid min-h-48 place-items-center border border-dashed border-hairline-strong px-5 text-center">
						<div><ShieldCheckIcon class="mx-auto size-6 text-text-faint" /><p class="mt-2 text-sm font-medium">Select or upload a backup</p><p class="mt-1 text-xs text-text-muted">Inspection never changes live state.</p></div>
					</div>
				{:else if selected.manifest}
					<div>
						<h3 class="text-base font-semibold">{selected.fileName}</h3>
						<p class="mt-1 flex items-center gap-1.5 font-mono text-2xs text-healthy">
							<CheckCircleIcon class="size-3.5 shrink-0" /> Verified SHA-256 {selected.archiveSha256}
						</p>
						{#if selected.progress}
							<p class="mt-1 font-mono text-2xs text-text-faint">
								Inspection {Math.floor(selected.progress.completedPerMille / 10)}% · {formatBytes(selected.archiveBytes)}
							</p>
						{/if}
						<ul class="mt-3 grid gap-2 sm:grid-cols-2">
							{#each selected.manifest.sections as section (section.path)}
								<li class="flex items-start gap-2 border border-hairline px-3 py-2">
									<CheckCircleIcon class="mt-0.5 size-3.5 shrink-0 text-healthy" />
									<span><span class="block text-sm capitalize">{sectionLabel(section.section)}</span><span class="font-mono text-2xs text-text-faint">verified · schema {section.schemaVersion} · {formatBytes(section.bytes)}</span></span>
								</li>
							{/each}
						</ul>
					</div>
					<div class="space-y-3 border-t border-hairline pt-4">
						<h3 class="text-sm font-semibold">Target paths</h3>
						{#each selected.manifest.sourcePaths as source (source.kind)}
							<label class="grid gap-1 text-xs font-medium" for={`backup-path-${source.kind}`}>
								<span class="capitalize">{pathLabel(source.kind)}</span>
								<Input id={`backup-path-${source.kind}`} value={mappings[source.kind] ?? ''} oninput={(event) => (mappings = { ...mappings, [source.kind]: event.currentTarget.value })} />
								<span class="truncate font-mono text-2xs font-normal text-text-faint">Source: {source.path}</span>
							</label>
						{/each}
						<Button variant="outline" onclick={() => void createPlan()} disabled={action !== null}>Run dry check</Button>
					</div>
					{#if plan}
						<div class="space-y-3 border-t border-hairline pt-4">
							<h3 class="text-sm font-semibold">Dry-run result</h3>
							{#if plan.issues.length > 0}
								<ul class="space-y-1 text-xs">{#each plan.issues as issue}<li class={issueClass(issue.severity)}><span class="font-medium capitalize">{issueLabel(issue.severity)}:</span> {issue.message}</li>{/each}</ul>
							{:else}<p class="text-sm text-healthy">All selected sections are ready to stage.</p>{/if}
							{#if plan.requiredSecretReferences.length > 0}<p class="text-xs text-text-muted">External secrets required: {plan.requiredSecretReferences.join(', ')}</p>{/if}
							<label class="flex items-start gap-2 text-xs"><input class="mt-0.5" type="checkbox" bind:checked={confirmed} disabled={!plan.canActivate} /><span>I understand that activation restarts the recorder and retains a 30-minute rollback point.</span></label>
							<Button onclick={() => void activate()} disabled={!confirmed || !plan.canActivate || action !== null}>Stage restore</Button>
						</div>
					{/if}
				{/if}
			</div>
		</div>
	</section>
{/if}
