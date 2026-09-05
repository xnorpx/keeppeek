<script lang="ts">
	import { onDestroy } from 'svelte';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import UploadIcon from '@lucide/svelte/icons/upload';
	import { maximumConfigurationArchiveBytes } from '$lib/backup-http-client';
	import type { ControlClient } from '$lib/control-client';
	import type { RestoreRecord } from '$lib/proto/backup_pb';
	import { Button } from './ui/button/index.js';

	type Props = {
		controller: ControlClient;
		onrestart: () => void | Promise<void>;
	};

	let { controller, onrestart }: Props = $props();
	let administrator = $state(false);
	let supported = $state(false);
	let action = $state<string | null>(null);
	let error = $state<string | null>(null);
	let uploadedFile = $state.raw<File | null>(null);
	let staged = $state.raw<RestoreRecord | null>(null);
	let confirmed = $state(false);
	let validFile = $derived(
		uploadedFile !== null &&
			uploadedFile.size > 0 &&
			uploadedFile.size <= maximumConfigurationArchiveBytes
	);
	let transfer: AbortController | null = null;
	let downloadUrl: string | null = null;

	$effect(() => {
		return controller.onAccessState((state) => {
			administrator = state.session?.role === 'administrator';
			if (!administrator) transfer?.abort();
		});
	});

	$effect(() => {
		return controller.onCapabilities((capabilityIds) => {
			supported = capabilityIds.includes('keeppeek.backup.v1');
			if (!supported) transfer?.abort();
		});
	});

	onDestroy(() => {
		transfer?.abort();
		if (downloadUrl) URL.revokeObjectURL(downloadUrl);
	});

	function selectArchive(event: Event): void {
		if (!(event.currentTarget instanceof HTMLInputElement)) return;
		uploadedFile = event.currentTarget.files?.[0] ?? null;
		confirmed = false;
		error = null;
		if (uploadedFile?.size === 0) error = 'The configuration ZIP is empty.';
		else if (uploadedFile && uploadedFile.size > maximumConfigurationArchiveBytes) {
			error = 'The configuration ZIP exceeds the 1 GiB limit.';
		}
	}

	async function exportConfiguration(): Promise<void> {
		await run('Exporting configuration', async (signal) => {
			const result = await controller.exportConfiguration(signal);
			if (signal.aborted) return;
			const url = URL.createObjectURL(result.blob);
			downloadUrl = url;
			const anchor = document.createElement('a');
			anchor.href = url;
			anchor.download = result.fileName;
			anchor.click();
			setTimeout(() => {
				URL.revokeObjectURL(url);
				if (downloadUrl === url) downloadUrl = null;
			}, 0);
		});
	}

	async function applyConfiguration(): Promise<void> {
		const file = uploadedFile;
		if (!file || !validFile || !confirmed || staged) return;
		await run('Applying configuration', async (signal) => {
			const record = await controller.applyConfiguration(file, signal);
			if (!signal.aborted) staged = record;
		});
	}

	async function run(
		label: string,
		operation: (signal: AbortSignal) => Promise<void>
	): Promise<void> {
		if (action || !administrator || !supported) return;
		const pending = new AbortController();
		transfer = pending;
		action = label;
		error = null;
		try {
			await operation(pending.signal);
		} catch (cause) {
			if (!pending.signal.aborted) {
				error = cause instanceof Error && cause.message ? cause.message : `${label} failed.`;
			}
		} finally {
			transfer = null;
			action = null;
		}
	}
</script>

{#if administrator && supported}
	<section
		id="backups"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
		aria-labelledby="backup-heading"
	>
		<header class="border-b border-hairline px-5 py-5">
			<h2 id="backup-heading" class="text-xl font-semibold">Backup and restore</h2>
			<p class="mt-1 font-mono text-xs text-text-muted">config.toml + secrets.toml</p>
		</header>
		<div
			class="flex items-start gap-2 border-b border-activity/30 bg-activity/5 px-5 py-3 text-xs leading-5 text-text-muted"
			role="note"
		>
			<TriangleAlertIcon class="mt-0.5 size-4 shrink-0 text-activity" />
			<p>
				<span class="font-medium text-text">Backup ZIPs contain plaintext secrets.</span>
				Restrict access and delete copies you no longer need. Databases and recordings are not included.
			</p>
		</div>

		{#if error}
			<p
				class="border-b border-destructive/30 bg-destructive/5 px-5 py-3 text-sm text-destructive"
				role="alert"
			>
				{error}
			</p>
		{/if}
		{#if action}
			<p class="border-b border-hairline px-5 py-2 font-mono text-xs text-text-muted" role="status">
				{action}...
			</p>
		{/if}

		<div class="flex flex-wrap items-center gap-3 border-b border-hairline px-5 py-4">
			<Button
				variant="outline"
				onclick={() => void exportConfiguration()}
				disabled={action !== null}
			>
				<DownloadIcon /> Export ZIP
			</Button>
		</div>
		<div class="min-w-0 space-y-4 p-5">
			<label class="grid min-w-0 gap-2 text-sm font-medium" for="configuration-archive">
				Configuration ZIP
				<input
					id="configuration-archive"
					type="file"
					accept=".zip,application/zip"
					class="block w-full min-w-0 text-xs text-text-muted file:mr-3 file:rounded-sm file:border file:border-hairline-strong file:bg-raised file:px-3 file:py-2 file:text-sm file:font-medium file:text-text focus-visible:outline-2 focus-visible:outline-ring"
					onchange={selectArchive}
					disabled={action !== null || staged !== null}
				/>
			</label>
			{#if uploadedFile}
				<p class="font-mono text-xs break-all text-text-muted">{uploadedFile.name}</p>
			{/if}
			<label class="flex items-start gap-2 text-xs leading-5">
				<input
					type="checkbox"
					class="mt-1 shrink-0"
					bind:checked={confirmed}
					disabled={!validFile || action !== null || staged !== null}
				/>
				<span
					>Replace <span class="font-mono">config.toml</span> and
					<span class="font-mono">secrets.toml</span> on restart.</span
				>
			</label>
			<Button
				onclick={() => void applyConfiguration()}
				disabled={!validFile || !confirmed || action !== null || staged !== null}
			>
				<UploadIcon /> Apply configuration
			</Button>
			{#if staged}
				<div class="space-y-3 border-t border-hairline pt-4">
					<p class="text-sm text-healthy" role="status">Configuration staged. Restart required.</p>
					<Button
						onclick={() =>
							void run('Restarting', async () => {
								await onrestart();
							})}
						disabled={action !== null}
					>
						<RotateCcwIcon /> Restart to apply
					</Button>
				</div>
			{/if}
		</div>
	</section>
{/if}
