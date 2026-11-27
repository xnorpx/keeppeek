<script lang="ts">
	import { onDestroy, type ComponentProps } from 'svelte';
	import { create } from '@bufbuild/protobuf';
	import type { AccessConnectionState } from '$lib/access';
	import BackupRestoreSection from '$lib/components/BackupRestoreSection.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import { RestoreRecordSchema, RestoreState, type RestoreRecord } from '$lib/proto/backup_pb';
	import CheckCircle2Icon from '@lucide/svelte/icons/circle-check-big';
	import FileArchiveIcon from '@lucide/svelte/icons/file-archive';
	import ShieldAlertIcon from '@lucide/svelte/icons/shield-alert';

	type BackupController = ComponentProps<typeof BackupRestoreSection>['controller'];

	const settingsNav = [
		'Server',
		'Storage & retention',
		'Runtime profiles',
		'Event sources',
		'Backup and restore',
		'Access',
		'Notifications',
		'Integrations',
		'Appearance & logs'
	] as const;

	const localAdminAccess: AccessConnectionState = {
		status: 'authenticated',
		generation: 1,
		message: null,
		session: {
			id: 'demo-local-session',
			principalId: 'local-admin',
			displayName: 'Local Administrator',
			role: 'administrator',
			local: true,
			clientClassification: 'browser-local',
			createdAtMs: Date.parse('2026-08-18T06:00:00Z'),
			lastActivityAtMs: Date.parse('2026-08-18T06:37:00Z'),
			absoluteExpiresAtMs: Date.parse('2026-08-19T06:00:00Z'),
			credentialExpiresAtMs: null
		}
	};

	let backupSessionKey = $state(0);
	let exportNotice = $state<string | null>(null);
	let restartNotice = $state<string | null>(null);
	let restartTimer: ReturnType<typeof setTimeout> | null = null;

	onDestroy(() => {
		if (restartTimer !== null) clearTimeout(restartTimer);
	});

	function unsupportedRemoteVerification(): Promise<never> {
		return Promise.reject(new Error('Remote verification is unused in local administrator demo.'));
	}

	const controller: BackupController = {
		onAccessState(listener) {
			listener(localAdminAccess);
			return () => {};
		},
		onCapabilities(listener) {
			listener(['keeppeek.backup.v1']);
			return () => {};
		},
		async exportConfiguration() {
			const fileName = 'keeppeek-config-2026-08-18.zip';
			exportNotice = `Exported ${fileName} (format-3, 4.8 KiB) · live TOML files untouched.`;
			return {
				blob: new Blob(['PK\x03\x04keeppeek-format-3-config-export'], {
					type: 'application/zip'
				}),
				fileName
			};
		},
		async applyConfiguration(file: File): Promise<RestoreRecord> {
			if (file.name.includes('tampered')) {
				throw new Error(
					'HTTP 400: Manifest checksum mismatch. Use an unmodified format-3 export; live files remain untouched.'
				);
			}
			return create(RestoreRecordSchema, {
				restoreId: 'rst-20260818-01',
				planId: 'plan-config-zip-01',
				backupId: 'bkp-config-zip-01',
				state: RestoreState.AWAITING_RESTART,
				createdAtUnixMs: BigInt(Date.parse('2026-08-18T06:37:00Z')),
				updatedAtUnixMs: BigInt(Date.parse('2026-08-18T06:37:00Z')),
				targetRevision: 'rev-8f42c19b7a01',
				restartRequired: true
			});
		},
		prepareConfigurationRestoreVerification: unsupportedRemoteVerification,
		getConfigurationRestoreVerification: unsupportedRemoteVerification,
		confirmConfigurationRestoreVerification: unsupportedRemoteVerification,
		prepareAdministratorVerification: unsupportedRemoteVerification,
		getAdministratorVerification: unsupportedRemoteVerification,
		verifyAdministratorBearer: unsupportedRemoteVerification,
		applyExternalAuthentication: unsupportedRemoteVerification
	};

	async function handleRestart(): Promise<void> {
		await new Promise<void>((resolveTimer) => {
			restartTimer = setTimeout(() => {
				restartTimer = null;
				resolveTimer();
			}, 250);
		});
		backupSessionKey += 1;
		restartNotice =
			'Staged configuration activated on restart. Live recorder verified config.toml and secrets.toml.';
	}

	function attachArchive(fileName: string, payload: string): void {
		const input = document.getElementById('configuration-archive');
		if (!(input instanceof HTMLInputElement)) return;
		const transfer = new DataTransfer();
		transfer.items.add(new File([payload], fileName, { type: 'application/zip' }));
		input.files = transfer.files;
		input.dispatchEvent(new Event('change', { bubbles: true }));
	}
</script>

<main
	data-paper-scenario="settings.desktop.appearance-system-logs"
	data-demo-config-zip
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="settings" />

	<aside
		class="flex w-[212px] shrink-0 flex-col border-r border-hairline bg-surface px-4 py-6"
		aria-label="Settings sections"
	>
		<p class="px-2 font-mono text-2xs tracking-caps text-text-faint">SETTINGS</p>
		<nav class="mt-3 flex flex-col gap-0.5">
			{#each settingsNav as item (item)}
				<span
					class="flex h-8 items-center border-l-2 px-2.5 text-xs {item === 'Backup and restore'
						? 'border-primary bg-primary/10 font-semibold text-foreground'
						: 'border-transparent text-text-muted'}"
				>
					{item}
				</span>
			{/each}
		</nav>
	</aside>

	<section class="flex min-w-0 flex-1 flex-col overflow-hidden" aria-label="Backup and restore">
		<header
			class="flex h-[56px] shrink-0 items-center justify-between border-b border-hairline bg-surface px-6"
		>
			<div>
				<h1 class="text-base font-semibold">Settings · Backup and restore</h1>
			</div>
			<span class="font-mono text-2xs tracking-caps text-text-faint">
				KEEPPEEK.BACKUP.V1 · FORMAT-3 ZIP
			</span>
		</header>

		<div class="grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)_336px] gap-6 overflow-hidden p-6">
			<div class="min-w-0 space-y-4">
				{#if exportNotice}
					<div
						class="flex items-center gap-2.5 rounded-sm border border-primary/35 bg-primary/5 px-4 py-2.5 text-xs text-text-muted"
					>
						<FileArchiveIcon class="size-4 shrink-0 text-primary-soft" />
						<span>{exportNotice}</span>
					</div>
				{/if}

				{#if restartNotice}
					<div
						class="flex items-center gap-2.5 rounded-sm border border-healthy/40 bg-healthy/5 px-4 py-2.5 text-xs text-healthy"
					>
						<CheckCircle2Icon class="size-4 shrink-0" />
						<span>{restartNotice}</span>
					</div>
				{/if}

				{#key backupSessionKey}
					<BackupRestoreSection {controller} onrestart={handleRestart} />
				{/key}
			</div>

			<aside class="flex flex-col gap-4">
				<section class="rounded-md border border-hairline bg-surface p-4">
					<p class="font-mono text-2xs tracking-caps text-primary-soft">FORMAT-3 ARCHIVE</p>
					<h2 class="mt-1 text-sm font-semibold">What the ZIP contains</h2>
					<dl class="mt-3 divide-y divide-hairline border-y border-hairline text-xs">
						<div class="flex items-center justify-between py-2">
							<dt class="text-text-muted">Root members</dt>
							<dd class="font-mono">config.toml · secrets.toml</dd>
						</div>
						<div class="flex items-center justify-between py-2">
							<dt class="text-text-muted">Integrity manifest</dt>
							<dd class="font-mono">ZIP comment SHA-256</dd>
						</div>
						<div class="flex items-center justify-between py-2">
							<dt class="text-text-muted">Target storage paths</dt>
							<dd class="font-mono text-healthy">Preserved</dd>
						</div>
						<div class="flex items-center justify-between py-2">
							<dt class="text-text-muted">Media &amp; catalog DBs</dt>
							<dd class="font-mono text-text-faint">Excluded</dd>
						</div>
						<div class="flex items-center justify-between py-2">
							<dt class="text-text-muted">Activation</dt>
							<dd class="font-mono">Staged until restart</dd>
						</div>
					</dl>
				</section>

				<section class="rounded-md border border-hairline bg-surface p-4">
					<div class="flex items-center gap-2">
						<ShieldAlertIcon class="size-4 text-activity" />
						<h2 class="text-sm font-semibold">Sample archives</h2>
					</div>
					<p class="mt-1.5 text-xs leading-5 text-text-muted">
						Attach an in-memory configuration ZIP to the file input above to test staging and
						checksum validation.
					</p>
					<div class="mt-3 flex flex-col gap-2">
						<button
							type="button"
							data-demo-action="attach-valid-zip"
							class="flex items-center justify-between rounded-sm border border-hairline-strong bg-raised px-3 py-2 text-left text-xs hover:bg-raised/80"
							onclick={() =>
								attachArchive(
									'keeppeek-config-2026-08-18.zip',
									'PK\x03\x04valid-format-3-manifest-verified'
								)}
						>
							<span class="font-mono">keeppeek-config-2026-08-18.zip</span>
							<span class="font-mono text-2xs text-healthy">VALID</span>
						</button>
						<button
							type="button"
							data-demo-action="attach-invalid-zip"
							class="flex items-center justify-between rounded-sm border border-hairline-strong bg-raised px-3 py-2 text-left text-xs hover:bg-raised/80"
							onclick={() =>
								attachArchive('tampered-config.zip', 'PK\x03\x04modified-toml-bad-checksum')}
						>
							<span class="font-mono">tampered-config.zip</span>
							<span class="font-mono text-2xs text-live-text">BAD HASH</span>
						</button>
					</div>
				</section>
			</aside>
		</div>
	</section>
</main>
