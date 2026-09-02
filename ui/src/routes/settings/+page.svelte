<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount, tick } from 'svelte';
	import { waitForMetricsAt } from '$lib/api';
	import { browserLogStore } from '$lib/browser-logs';
	import { downloadDiagnosticsBundle } from '$lib/diagnostics-bundle';
	import type {
		CameraCatalogInfo,
		SanitizedConfig,
		ServerHealthResponse,
		SettingsConfigUpdate
	} from '$lib/types';
	import * as Card from '$lib/components/ui/card/index.js';
	import AccessSection from '$lib/components/AccessSection.svelte';
	import BackupRestoreSection from '$lib/components/BackupRestoreSection.svelte';
	import MobileAccessSection from '$lib/components/MobileAccessSection.svelte';
	import MobileSettingsActionBar from '$lib/components/MobileSettingsActionBar.svelte';
	import MobileSettingsHeader from '$lib/components/MobileSettingsHeader.svelte';
	import AppearanceSystemSection from '$lib/components/AppearanceSystemSection.svelte';
	import EventSourcesSection from '$lib/components/EventSourcesSection.svelte';
	import GroupsSection from '$lib/components/GroupsSection.svelte';
	import IntegrationsSection from '$lib/components/IntegrationsSection.svelte';
	import MobileSettingsIndex from '$lib/components/MobileSettingsIndex.svelte';
	import NotificationsSection from '$lib/components/NotificationsSection.svelte';
	import PeekDashboardSettings from '$lib/components/PeekDashboardSettings.svelte';
	import StorageRetentionSection from '$lib/components/StorageRetentionSection.svelte';
	import StorageSettingsEditor from '$lib/components/StorageSettingsEditor.svelte';
	import SettingsApplyingState from '$lib/components/SettingsApplyingState.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import { Separator } from '$lib/components/ui/separator/index.js';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import SaveIcon from '@lucide/svelte/icons/save';
	import TerminalIcon from '@lucide/svelte/icons/terminal';
	import { mobileSettingsFocus, type MobileSettingsRenderTarget } from '$lib/mobile-settings';
	import { useCapabilityState } from '$lib/capability-context';
	import { useControlClient } from '$lib/control-context';

	type RuntimeEditorMode = 'server' | 'storage' | null;

	type RuntimeSettingsForm = {
		host: string;
		port: string;
	};

	const controlClient = useControlClient();
	const capabilities = useCapabilityState();

	let config = $state.raw<SanitizedConfig | null>(null);
	let serverHealth = $state.raw<ServerHealthResponse | null>(null);
	let serverHealthError = $state<string | null>(null);
	let catalogInfo = $state.raw<CameraCatalogInfo | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let restartError = $state<string | null>(null);
	let statusMessage = $state<string | null>(null);
	let restarting = $state(false);
	let pendingRestart = $state(false);
	let pendingStorageMigration = $state(false);
	let runtimeEditor = $state<RuntimeEditorMode>(null);
	let savingRuntimeSettings = $state(false);
	let downloadingDiagnostics = $state(false);
	let diagnosticsError = $state<string | null>(null);
	let runtimeSettingsError = $state<string | null>(null);
	let runtimeSettingsForm = $state<RuntimeSettingsForm>(emptyRuntimeSettingsForm());
	let restartTargetOrigin = $state<string | null>(null);
	let administrator = $state(false);
	let backupAvailable = $derived(administrator && capabilities.supports('keeppeek.backup.v1'));
	let mobileFocus = $derived(mobileSettingsFocus(page.url.hash, backupAvailable));

	onMount(() => {
		if (window.location.hash === '#camera-defaults') {
			window.location.replace(resolve('/cameras'));
			return;
		}
		void loadSettings();
		const closeAccessState = controlClient.onAccessState((state) => {
			administrator = state.session?.role === 'administrator';
		});
		const handleHashChange = () => void scrollToHashTarget();
		window.addEventListener('hashchange', handleHashChange);
		return () => {
			closeAccessState();
			window.removeEventListener('hashchange', handleHashChange);
		};
	});

	function emptyRuntimeSettingsForm(): RuntimeSettingsForm {
		return {
			host: '',
			port: ''
		};
	}

	function beginMobileAccessCredential(): void {
		document
			.querySelector<HTMLButtonElement>('[data-mobile-access] [data-new-access-credential]')
			?.click();
	}

	function runtimeSettingsFormFromConfig(config: SanitizedConfig): RuntimeSettingsForm {
		return {
			host: config.host,
			port: config.port.toString()
		};
	}

	function restartOrigin(config: SanitizedConfig): string {
		const host =
			config.host === '0.0.0.0' || config.host === '::' || config.host === '[::]'
				? window.location.hostname
				: config.host;
		const formattedHost = host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
		return new URL(`${window.location.protocol}//${formattedHost}:${config.port}`).origin;
	}

	async function loadSettings() {
		loading = true;
		error = null;
		try {
			const healthRequest = controlClient.getHealth().then(
				(value) => ({ value, error: null }),
				(cause: unknown) => ({
					value: null,
					error: cause instanceof Error ? cause.message : 'Storage health is unavailable.'
				})
			);
			const catalogRequest = controlClient.getCameraCatalog().catch(() => null);
			const [nextConfig, nextHealth, nextCatalogInfo] = await Promise.all([
				controlClient.getRuntimeConfiguration(),
				healthRequest,
				catalogRequest
			]);
			config = nextConfig;
			serverHealth = nextHealth.value;
			serverHealthError = nextHealth.error;
			catalogInfo = nextCatalogInfo;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to load settings.';
		} finally {
			loading = false;
		}
		await scrollToHashTarget();
		if (config && page.url.searchParams.get('edit') === 'storage') {
			await openStorageSettings();
		}
	}

	async function exportDiagnostics(): Promise<void> {
		if (downloadingDiagnostics) return;
		downloadingDiagnostics = true;
		diagnosticsError = null;
		try {
			await downloadDiagnosticsBundle(controlClient, browserLogStore.snapshot());
		} catch (cause) {
			diagnosticsError =
				cause instanceof Error ? cause.message : 'Unable to create the diagnostics package.';
		} finally {
			downloadingDiagnostics = false;
		}
	}

	async function scrollToHashTarget(): Promise<void> {
		await tick();
		const targetId = window.location.hash.slice(1);
		if (
			![
				'dashboards',
				'backups',
				'storage',
				'event-sources',
				'groups',
				'access',
				'integrations',
				'notifications',
				'appearance'
			].includes(targetId)
		)
			return;
		if (window.matchMedia('(max-width: 767px)').matches && mobileSettingsFocus(targetId)) {
			document.querySelector('[data-mobile-settings-focus]')?.scrollIntoView({ block: 'start' });
			return;
		}
		document.getElementById(targetId)?.scrollIntoView({ block: 'start' });
	}

	async function openServerSettings() {
		if (!config) return;
		runtimeSettingsForm = runtimeSettingsFormFromConfig(config);
		runtimeSettingsError = null;
		runtimeEditor = 'server';
		await tick();
		const runtimeForm = document.getElementById('runtime-settings-form');
		runtimeForm?.scrollIntoView({
			behavior: 'smooth',
			block: 'start'
		});
		runtimeForm?.querySelector<HTMLInputElement>('#server-host')?.focus({ preventScroll: true });
	}

	async function openStorageSettings() {
		if (!config) return;
		runtimeSettingsError = null;
		runtimeEditor = 'storage';
		await tick();
		const storageEditor = document.getElementById('storage-settings-editor');
		storageEditor?.scrollIntoView({ behavior: 'smooth', block: 'start' });
		storageEditor
			?.querySelector<HTMLInputElement>('#recording-location')
			?.focus({ preventScroll: true });
	}

	function closeRuntimeSettings() {
		runtimeEditor = null;
		runtimeSettingsError = null;
		runtimeSettingsForm = emptyRuntimeSettingsForm();
	}

	function parseWholeNumber(
		value: string,
		label: string,
		minimum: number,
		maximum: number
	): number {
		const trimmed = value.trim();
		if (!/^\d+$/.test(trimmed)) {
			throw new Error(`${label} must be a whole number.`);
		}
		const number = Number(trimmed);
		if (!Number.isSafeInteger(number) || number < minimum || number > maximum) {
			throw new Error(`${label} must be between ${minimum} and ${maximum}.`);
		}
		return number;
	}

	function runtimeSettingsUpdate(): SettingsConfigUpdate {
		if (!config) throw new Error('Runtime configuration is unavailable.');
		const host = runtimeSettingsForm.host.trim();
		if (!host || /\s/.test(host)) {
			throw new Error('Host must be a nonempty address or hostname.');
		}
		return {
			host,
			port: parseWholeNumber(runtimeSettingsForm.port, 'Server port', 1, 65_535),
			expected_configuration_revision: config.configuration_revision,
			move_existing_recordings: false,
			storage: { ...config.storage }
		};
	}

	async function saveRuntimeSettings(event: SubmitEvent) {
		event.preventDefault();
		if (savingRuntimeSettings) return;
		let update: SettingsConfigUpdate;
		try {
			update = runtimeSettingsUpdate();
		} catch (cause) {
			runtimeSettingsError =
				cause instanceof Error ? cause.message : 'Server and storage settings are invalid.';
			return;
		}

		const submittedForm = { ...runtimeSettingsForm };
		savingRuntimeSettings = true;
		runtimeSettingsForm = runtimeSettingsFormFromConfig(config!);
		runtimeSettingsError = null;
		try {
			const result = await controlClient.updateRuntimeConfiguration(update);
			config = result.config;
			restartTargetOrigin = restartOrigin(result.config);
			pendingRestart ||= result.restart_required;
			statusMessage = 'Server settings saved.';
			closeRuntimeSettings();
		} catch (cause) {
			runtimeSettingsForm = submittedForm;
			runtimeSettingsError =
				cause instanceof Error ? cause.message : 'Server and storage settings were not saved.';
		} finally {
			savingRuntimeSettings = false;
		}
	}

	async function saveStorageSettings(update: SettingsConfigUpdate) {
		if (savingRuntimeSettings) return;
		savingRuntimeSettings = true;
		runtimeSettingsError = null;
		try {
			const result = await controlClient.updateRuntimeConfiguration(update);
			config = result.config;
			restartTargetOrigin = restartOrigin(result.config);
			pendingRestart ||= result.restart_required;
			pendingStorageMigration = update.move_existing_recordings;
			statusMessage = update.move_existing_recordings
				? 'Storage settings staged. Restart will move existing storage before recording resumes.'
				: 'Storage settings staged. Restart when you are ready to apply them.';
			closeRuntimeSettings();
		} catch (cause) {
			runtimeSettingsError =
				cause instanceof Error ? cause.message : 'Storage settings were not saved.';
		} finally {
			savingRuntimeSettings = false;
		}
	}

	function delay(milliseconds: number): Promise<void> {
		return new Promise((resolve) => window.setTimeout(resolve, milliseconds));
	}

	async function waitForRestart() {
		const targetOrigin = restartTargetOrigin ?? window.location.origin;
		await delay(500);
		for (let attempt = 0; attempt < 40; attempt += 1) {
			try {
				await waitForMetricsAt(targetOrigin);
				if (targetOrigin === window.location.origin) {
					window.location.reload();
				} else {
					window.location.assign(new URL('/settings', targetOrigin).toString());
				}
				return;
			} catch {
				await delay(500);
			}
		}
		throw new Error('The server is taking longer than expected to restart.');
	}

	async function applyChanges() {
		if (!pendingRestart || restarting) return;
		restarting = true;
		restartError = null;
		try {
			await controlClient.restartServer();
			await waitForRestart();
		} catch (cause) {
			restartError = cause instanceof Error ? cause.message : 'The server did not restart.';
		} finally {
			restarting = false;
		}
	}

	function handleSaveShortcut(event: KeyboardEvent): void {
		if (!(event.metaKey || event.ctrlKey) || event.altKey || event.key.toLowerCase() !== 's')
			return;
		event.preventDefault();
		if (runtimeEditor === 'server') {
			document.querySelector<HTMLFormElement>('#runtime-settings-form')?.requestSubmit();
		} else if (runtimeEditor === 'storage') {
			document.querySelector<HTMLFormElement>('#storage-settings-editor')?.requestSubmit();
		}
	}

	async function restartRecorder() {
		if (restarting) return;
		restarting = true;
		restartError = null;
		statusMessage = null;
		try {
			await controlClient.restartServer();
			await waitForRestart();
		} catch (cause) {
			restartError = cause instanceof Error ? cause.message : 'The server did not restart.';
		} finally {
			restarting = false;
		}
	}

	function mobileSectionClass(target: MobileSettingsRenderTarget): string {
		if (mobileFocus?.renderTarget !== target) return 'max-md:hidden';
		return target === 'access' ? '' : 'max-md:mx-4';
	}

	function mobileFocusTrailing(target: MobileSettingsRenderTarget): string | undefined {
		if (target === 'access') return 'Target · identity v1';
		return undefined;
	}
</script>

<svelte:window onkeydowncapture={handleSaveShortcut} />

<svelte:head>
	<title>Settings - KeepPeek</title>
</svelte:head>

<div class="space-y-6">
	<h1 class="hidden text-2xl font-bold tracking-tight md:block">Settings</h1>

	{#if loading}
		<div class="max-w-[1310px] space-y-6">
			{#each [0, 1, 2] as skeleton (skeleton)}
				<Card.Root>
					<Card.Header>
						<Skeleton class="h-5 w-32" />
					</Card.Header>
					<Card.Content class="space-y-3 pb-6">
						<Skeleton class="h-9 w-full" />
						<Skeleton class="h-14 w-full" />
					</Card.Content>
				</Card.Root>
			{/each}
		</div>
	{:else if error}
		<Card.Root class="max-w-2xl border-destructive">
			<Card.Header>
				<Card.Title>Settings unavailable</Card.Title>
			</Card.Header>
			<Card.Content class="flex flex-wrap items-center justify-between gap-3 pb-6">
				<p class="text-sm text-destructive">{error}</p>
				<Button variant="outline" onclick={() => void loadSettings()}>
					<RefreshCwIcon />
					Retry
				</Button>
			</Card.Content>
		</Card.Root>
	{:else if config}
		<div class="max-w-[1310px] space-y-6">
			<div class="md:hidden">
				{#if mobileFocus}
					<div data-mobile-settings-focus>
						<MobileSettingsHeader
							title={mobileFocus.label}
							backHref={resolve('/settings')}
							trailing={mobileFocusTrailing(mobileFocus.renderTarget!)}
						/>
					</div>
				{:else}
					<MobileSettingsIndex {config} {backupAvailable} />
				{/if}
			</div>

			{#if pendingRestart || statusMessage || restartError}
				<div
					class="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border bg-muted/45 px-3 py-2 text-sm {mobileFocus
						? 'max-md:mx-4'
						: 'max-md:hidden'}"
					role="status"
				>
					<p class={restartError ? 'text-destructive' : 'text-muted-foreground'}>
						{restartError ??
							(restarting && pendingStorageMigration
								? 'Restarting KeepPeek. Existing storage moves before recording resumes.'
								: (statusMessage ?? 'Saved changes are ready to apply.'))}
					</p>
					{#if pendingRestart}
						<Button size="sm" onclick={applyChanges} disabled={restarting}>
							<RotateCcwIcon class={restarting ? 'animate-spin' : undefined} />
							{restarting
								? pendingStorageMigration
									? 'Restarting and moving storage'
									: 'Applying changes'
								: pendingStorageMigration
									? 'Restart and move storage'
									: 'Apply changes'}
						</Button>
					{/if}
				</div>
			{/if}

			<div class={mobileSectionClass('dashboards')}>
				<PeekDashboardSettings controller={controlClient} health={serverHealth} />
			</div>

			<Card.Root class="hidden md:grid">
				<Card.Header>
					<Card.Title>Diagnostics</Card.Title>
					<Card.Description>Inspect live server and browser logs.</Card.Description>
					<Card.Action>
						<Button href={resolve('/settings/logs')} variant="outline" size="sm">
							<TerminalIcon />
							View logs
						</Button>
					</Card.Action>
				</Card.Header>
			</Card.Root>

			<div class={mobileSectionClass('storage')}>
				<StorageRetentionSection
					{config}
					health={serverHealth}
					healthError={serverHealthError}
					onedit={() => void openStorageSettings()}
				/>
				{#if runtimeEditor === 'storage'}
					<div class="mt-4">
						<StorageSettingsEditor
							{config}
							health={serverHealth}
							saving={savingRuntimeSettings}
							error={runtimeSettingsError}
							oncancel={closeRuntimeSettings}
							onsave={saveStorageSettings}
						/>
					</div>
				{/if}
			</div>

			<div class={mobileSectionClass('event-sources')}>
				<EventSourcesSection health={serverHealth} healthError={serverHealthError} />
			</div>

			<div class={mobileSectionClass('backups')}>
				<BackupRestoreSection controller={controlClient} onrestart={restartRecorder} />
			</div>

			<div class={mobileSectionClass('groups')}>
				<GroupsSection />
			</div>

			<div class={mobileSectionClass('access')}>
				<MobileAccessSection
					controller={controlClient}
					onrevealaccesskey={() => controlClient.revealAccessKey()}
					onrotateaccesskey={() => controlClient.rotateAccessKey()}
				/>
				<div class="hidden md:block">
					<AccessSection
						controller={controlClient}
						onrevealaccesskey={() => controlClient.revealAccessKey()}
						onrotateaccesskey={() => controlClient.rotateAccessKey()}
					/>
				</div>
			</div>

			{#if mobileFocus?.id === 'access'}
				<MobileSettingsActionBar
					action="New token"
					capability="keeppeek.identity.v1"
					onaction={beginMobileAccessCredential}
				/>
			{/if}

			<div class={mobileSectionClass('integrations')}>
				<IntegrationsSection />
			</div>

			<div class={mobileSectionClass('notifications')}>
				<NotificationsSection />
			</div>

			<div class={mobileSectionClass('appearance')}>
				<AppearanceSystemSection
					health={serverHealth}
					healthError={serverHealthError}
					{catalogInfo}
					{restarting}
					{downloadingDiagnostics}
					{diagnosticsError}
					onrestart={() => void restartRecorder()}
					ondownloaddiagnostics={exportDiagnostics}
				/>
			</div>

			<form
				id="runtime-settings-form"
				class="scroll-mt-4 space-y-4 {runtimeEditor === 'server' ? 'block' : 'hidden md:block'}"
				onsubmit={saveRuntimeSettings}
			>
				<fieldset disabled={savingRuntimeSettings} class="contents">
					<div class="max-w-2xl">
						<Card.Root id="server-settings">
							<Card.Header>
								<Card.Title>Server</Card.Title>
								{#if runtimeEditor !== 'server'}
									<Card.Action>
										<Button variant="outline" size="sm" onclick={() => void openServerSettings()}>
											<PencilIcon />
											Edit server
										</Button>
									</Card.Action>
								{/if}
							</Card.Header>
							<Card.Content class="pb-6">
								{#if runtimeEditor === 'server'}
									<div class="grid gap-4">
										<label class="grid gap-1.5 text-sm font-medium" for="server-host">
											Host
											<Input
												id="server-host"
												bind:value={runtimeSettingsForm.host}
												autocomplete="off"
											/>
										</label>
										<label class="grid gap-1.5 text-sm font-medium" for="server-port">
											Port
											<Input
												id="server-port"
												bind:value={runtimeSettingsForm.port}
												inputmode="numeric"
												autocomplete="off"
											/>
										</label>
									</div>
								{:else}
									<dl class="space-y-3 text-sm">
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Host</dt>
											<dd class="text-right">{config.host}</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Port</dt>
											<dd>{config.port}</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Running cameras</dt>
											<dd>{config.camera_count}</dd>
										</div>
									</dl>
								{/if}
							</Card.Content>
						</Card.Root>
					</div>
				</fieldset>
				{#if runtimeEditor === 'server'}
					{#if savingRuntimeSettings}
						<SettingsApplyingState
							fieldLabel="Server port"
							confirmedValue={String(config?.port ?? '')}
							actionLabel="Applying server settings"
							lockLabel="Fields locked until server responds"
							detail="Confirmed values remain visible and fields stay locked until the server responds."
							class="rounded-sm border border-activity/45"
						/>
					{/if}
					{#if runtimeSettingsError}
						<p class="text-sm text-destructive" role="alert">{runtimeSettingsError}</p>
					{/if}
					<div class="flex justify-end gap-2">
						<Button
							variant="outline"
							onclick={closeRuntimeSettings}
							disabled={savingRuntimeSettings}>Cancel</Button
						>
						<Button type="submit" disabled={savingRuntimeSettings}>
							{#if !savingRuntimeSettings}<SaveIcon />{:else}<RefreshCwIcon
									class="animate-spin"
								/>{/if}
							{savingRuntimeSettings ? 'Saving' : 'Save server settings'}
						</Button>
					</div>
				{/if}
			</form>
		</div>
	{/if}
</div>
