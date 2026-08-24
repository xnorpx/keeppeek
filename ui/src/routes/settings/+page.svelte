<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount, tick } from 'svelte';
	import { waitForMetricsAt } from '$lib/api';
	import type {
		CameraBackend,
		CameraRecordingMode,
		CameraCatalogInfo,
		CameraSettings,
		CameraSettingsUpdate,
		CameraTransport,
		DiscoveredCameraSettings,
		SanitizedConfig,
		ServerHealthResponse,
		SettingsConfigUpdate
	} from '$lib/types';
	import * as Card from '$lib/components/ui/card/index.js';
	import CameraDefaultsSection from '$lib/components/CameraDefaultsSection.svelte';
	import AccessSection from '$lib/components/AccessSection.svelte';
	import MobileAccessSection from '$lib/components/MobileAccessSection.svelte';
	import MobileCameraDefaultsSection from '$lib/components/MobileCameraDefaultsSection.svelte';
	import MobileSettingsActionBar from '$lib/components/MobileSettingsActionBar.svelte';
	import MobileSettingsHeader from '$lib/components/MobileSettingsHeader.svelte';
	import AppearanceSystemSection from '$lib/components/AppearanceSystemSection.svelte';
	import EventSourcesSection from '$lib/components/EventSourcesSection.svelte';
	import GroupsSection from '$lib/components/GroupsSection.svelte';
	import IntegrationsSection from '$lib/components/IntegrationsSection.svelte';
	import MobileSettingsIndex from '$lib/components/MobileSettingsIndex.svelte';
	import NotificationsSection from '$lib/components/NotificationsSection.svelte';
	import StorageRetentionSection from '$lib/components/StorageRetentionSection.svelte';
	import StorageSettingsEditor from '$lib/components/StorageSettingsEditor.svelte';
	import SettingsApplyingState from '$lib/components/SettingsApplyingState.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import { Separator } from '$lib/components/ui/separator/index.js';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import SaveIcon from '@lucide/svelte/icons/save';
	import SearchIcon from '@lucide/svelte/icons/search';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import TerminalIcon from '@lucide/svelte/icons/terminal';
	import XIcon from '@lucide/svelte/icons/x';
	import { mobileSettingsFocus, type MobileSettingsRenderTarget } from '$lib/mobile-settings';
	import { useControlClient } from '$lib/control-context';

	type EditorMode = 'new' | 'edit' | null;
	type RuntimeEditorMode = 'server' | 'storage' | null;

	type CameraForm = {
		ip: string;
		displayName: string;
		manufacturer: string;
		username: string;
		password: string;
		onvifPort: string;
		httpPort: string;
		mainRtspUrl: string;
		subRtspUrl: string;
		uid: string;
		clearUid: boolean;
		backend: CameraBackend;
		transport: CameraTransport;
		recordGenericMotionEvents: boolean;
		recordingMode: CameraRecordingMode;
		eventRecordingDurationSeconds: string;
	};

	type RuntimeSettingsForm = {
		host: string;
		port: string;
	};

	const selectClass =
		'border-input bg-background ring-offset-background focus-visible:border-ring focus-visible:ring-ring/50 h-9 w-full rounded-md border px-3 text-sm font-medium shadow-xs outline-none focus-visible:ring-[3px]';
	const DEFAULT_REOLINK_ONVIF_PORT = 8000;
	const DEFAULT_REOLINK_HTTP_PORT = 80;
	const controlClient = useControlClient();

	let config = $state.raw<SanitizedConfig | null>(null);
	let serverHealth = $state.raw<ServerHealthResponse | null>(null);
	let serverHealthError = $state<string | null>(null);
	let catalogInfo = $state.raw<CameraCatalogInfo | null>(null);
	let cameras = $state.raw<CameraSettings[]>([]);
	let discovered = $state.raw<DiscoveredCameraSettings[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let discoveryError = $state<string | null>(null);
	let editorError = $state<string | null>(null);
	let restartError = $state<string | null>(null);
	let statusMessage = $state<string | null>(null);
	let subnetPrefixes = $state('');
	let didDiscover = $state(false);
	let discovering = $state(false);
	let saving = $state(false);
	let removingIp = $state<string | null>(null);
	let restarting = $state(false);
	let pendingRestart = $state(false);
	let pendingStorageMigration = $state(false);
	let editorMode = $state<EditorMode>(null);
	let editorIp = $state<string | null>(null);
	let form = $state<CameraForm>(emptyForm());
	let cameraEditor = $state<HTMLFormElement | null>(null);
	let runtimeEditor = $state<RuntimeEditorMode>(null);
	let savingRuntimeSettings = $state(false);
	let runtimeSettingsError = $state<string | null>(null);
	let runtimeSettingsForm = $state<RuntimeSettingsForm>(emptyRuntimeSettingsForm());
	let restartTargetOrigin = $state<string | null>(null);
	let isNewCamera = $derived(editorMode === 'new');
	let mobileFocus = $derived(mobileSettingsFocus(page.url.hash));

	onMount(() => {
		void loadSettings();
		const handleHashChange = () => void scrollToHashTarget();
		window.addEventListener('hashchange', handleHashChange);
		return () => window.removeEventListener('hashchange', handleHashChange);
	});

	function emptyForm(): CameraForm {
		return {
			ip: '',
			displayName: '',
			manufacturer: '',
			username: '',
			password: '',
			onvifPort: '',
			httpPort: '',
			mainRtspUrl: '',
			subRtspUrl: '',
			uid: '',
			clearUid: false,
			backend: 'auto',
			transport: 'tcp',
			recordGenericMotionEvents: false,
			recordingMode: 'event-boost',
			eventRecordingDurationSeconds: '60'
		};
	}

	function formFromCamera(camera: CameraSettings): CameraForm {
		return {
			ip: camera.ip,
			displayName: camera.display_name ?? '',
			manufacturer: camera.manufacturer_override ?? '',
			username: '',
			password: '',
			onvifPort: camera.onvif_port?.toString() ?? '',
			httpPort: camera.http_port?.toString() ?? '',
			mainRtspUrl: camera.main_rtsp_url ?? '',
			subRtspUrl: camera.sub_rtsp_url ?? '',
			uid: '',
			clearUid: false,
			backend: camera.backend,
			transport: camera.transport,
			recordGenericMotionEvents: camera.record_generic_motion_events,
			recordingMode: camera.recording_mode,
			eventRecordingDurationSeconds: camera.event_recording_duration_secs.toString()
		};
	}

	function emptyRuntimeSettingsForm(): RuntimeSettingsForm {
		return {
			host: '',
			port: ''
		};
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
			const [nextConfig, nextCameras, nextHealth, nextCatalogInfo] = await Promise.all([
				controlClient.getRuntimeConfiguration(),
				controlClient.getCameraSettings(),
				healthRequest,
				catalogRequest
			]);
			config = nextConfig;
			cameras = nextCameras;
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

	async function scrollToHashTarget(): Promise<void> {
		await tick();
		const targetId = window.location.hash.slice(1);
		if (
			![
				'storage',
				'camera-defaults',
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

	function parseSubnetPrefixes(): number[] {
		const values = subnetPrefixes
			.split(',')
			.map((value) => value.trim())
			.filter(Boolean);
		const subnets = values.map((value) => {
			const octets = value.split('.');
			if (
				octets.length !== 3 ||
				octets.some((octet) => !/^\d+$/.test(octet) || Number(octet) > 255)
			) {
				throw new Error('Enter subnet prefixes such as 192.168.137.');
			}
			return Number(octets[2]);
		});
		const uniqueSubnets = [...new Set(subnets)];
		if (uniqueSubnets.length > 32) {
			throw new Error('At most 32 subnet prefixes can be scanned at once.');
		}
		return uniqueSubnets;
	}

	async function discoverCameras() {
		if (discovering) return;
		discovering = true;
		discoveryError = null;
		try {
			discovered = await controlClient.discoverCameras(parseSubnetPrefixes());
			didDiscover = true;
		} catch (cause) {
			discoveryError = cause instanceof Error ? cause.message : 'Camera discovery failed.';
		} finally {
			discovering = false;
		}
	}

	function openManualCamera() {
		form = emptyForm();
		editorMode = 'new';
		editorIp = null;
		editorError = null;
		void revealCameraEditor('camera-ip');
	}

	function editCamera(camera: CameraSettings) {
		form = formFromCamera(camera);
		editorMode = 'edit';
		editorIp = camera.ip;
		editorError = null;
		void revealCameraEditor('camera-name');
	}

	function configureDiscoveredCamera(camera: DiscoveredCameraSettings) {
		const configured = cameras.find((candidate) => candidate.ip === camera.ip);
		if (configured) {
			editCamera(configured);
			return;
		}
		form = {
			...emptyForm(),
			ip: camera.ip,
			displayName: camera.name ?? '',
			onvifPort:
				camera.onvif_port?.toString() ??
				(camera.brand.toLowerCase() === 'reolink' ? DEFAULT_REOLINK_ONVIF_PORT.toString() : ''),
			httpPort:
				camera.brand.toLowerCase() === 'reolink' ? DEFAULT_REOLINK_HTTP_PORT.toString() : '',
			backend: camera.brand.toLowerCase() === 'reolink' ? 'reo-proto' : 'auto'
		};
		editorMode = 'new';
		editorIp = null;
		editorError = null;
		void revealCameraEditor('camera-username');
	}

	function closeEditor() {
		editorMode = null;
		editorIp = null;
		editorError = null;
		form = emptyForm();
	}

	async function revealCameraEditor(focusId: 'camera-ip' | 'camera-name' | 'camera-username') {
		await tick();
		cameraEditor?.scrollIntoView({ behavior: 'smooth', block: 'start' });
		cameraEditor?.querySelector<HTMLInputElement>(`#${focusId}`)?.focus({ preventScroll: true });
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

	function parsePort(value: string, label: string): number | null {
		const trimmed = value.trim();
		if (!trimmed) return null;
		const port = Number(trimmed);
		if (!Number.isInteger(port) || port < 1 || port > 65_535) {
			throw new Error(`${label} must be a whole number from 1 to 65535.`);
		}
		return port;
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
			move_existing_recordings: false,
			storage: { ...config.storage }
		};
	}

	function updateFromForm(): CameraSettingsUpdate {
		const update: CameraSettingsUpdate = {
			display_name: form.displayName.trim() || null,
			manufacturer: form.manufacturer.trim() || null,
			onvif_port: parsePort(form.onvifPort, 'ONVIF port'),
			http_port: parsePort(form.httpPort, 'HTTP port'),
			main_rtsp_url: form.mainRtspUrl.trim() || null,
			sub_rtsp_url: form.subRtspUrl.trim() || null,
			backend: form.backend,
			transport: form.transport,
			record_generic_motion_events: form.recordGenericMotionEvents,
			recording_mode: form.recordingMode,
			event_recording_duration_secs: parseWholeNumber(
				form.eventRecordingDurationSeconds,
				'Event recording duration',
				1,
				3_600
			)
		};
		if (form.username.trim()) update.username = form.username.trim();
		if (form.password) update.password = form.password;
		if (form.uid.trim()) {
			update.uid = form.uid.trim();
		} else if (form.clearUid || isNewCamera) {
			update.uid = null;
		}
		return update;
	}

	async function saveCamera(event: SubmitEvent) {
		event.preventDefault();
		if (saving || !editorMode) return;
		const ip = form.ip.trim();
		if (!ip) {
			editorError = 'Camera IP address is required.';
			return;
		}
		if (isNewCamera && (!form.username.trim() || !form.password)) {
			editorError = 'Username and password are required for a new camera.';
			return;
		}

		let update: CameraSettingsUpdate;
		try {
			update = updateFromForm();
		} catch (cause) {
			editorError = cause instanceof Error ? cause.message : 'Camera configuration is invalid.';
			return;
		}

		saving = true;
		editorError = null;
		try {
			const result = await controlClient.updateCamera(ip, update);
			cameras = [
				...cameras.filter((camera) => camera.ip !== result.camera.ip),
				result.camera
			].toSorted((left, right) => left.ip.localeCompare(right.ip));
			discovered = discovered.map((camera) =>
				camera.ip === result.camera.ip ? { ...camera, configured: true } : camera
			);
			pendingRestart ||= result.restart_required;
			statusMessage = 'Camera settings saved.';
			closeEditor();
		} catch (cause) {
			editorError = cause instanceof Error ? cause.message : 'Camera settings were not saved.';
		} finally {
			saving = false;
		}
	}

	async function removeCamera(camera: CameraSettings) {
		if (removingIp || !window.confirm(`Remove ${cameraDisplayName(camera)}?`)) return;
		removingIp = camera.ip;
		statusMessage = null;
		try {
			await controlClient.removeCamera(camera.ip);
			cameras = cameras.filter((candidate) => candidate.ip !== camera.ip);
			discovered = discovered.map((candidate) =>
				candidate.ip === camera.ip ? { ...candidate, configured: false, health: null } : candidate
			);
			if (editorIp === camera.ip) closeEditor();
			pendingRestart = true;
			statusMessage = 'Camera removed. Apply changes to update the server.';
		} catch (cause) {
			editorError = cause instanceof Error ? cause.message : 'Camera settings were not removed.';
		} finally {
			removingIp = null;
		}
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
		if (editorMode && cameraEditor) {
			cameraEditor.requestSubmit();
			return;
		}
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

	function cameraDisplayName(camera: CameraSettings): string {
		return camera.display_name ?? camera.model ?? camera.ip;
	}

	function recordingModeLabel(mode: CameraRecordingMode): string {
		if (mode === 'off') return "Don't record";
		if (mode === 'sub') return 'Sub only';
		if (mode === 'main') return 'Main only';
		if (mode === 'both') return 'Main + sub';
		return 'Sub, main on events';
	}

	function liveCameraHref(cameraId: string): string {
		return `${resolve('/')}?camera=${encodeURIComponent(cameraId)}`;
	}

	function cameraInfoHref(cameraId: string): string {
		return `${resolve('/camera')}?camera=${encodeURIComponent(cameraId)}`;
	}

	function mobileSectionClass(target: MobileSettingsRenderTarget): string {
		if (mobileFocus?.renderTarget !== target) return 'max-md:hidden';
		return target === 'camera-defaults' || target === 'access' ? '' : 'max-md:mx-4';
	}

	function mobileFocusTrailing(target: MobileSettingsRenderTarget): string | undefined {
		if (target === 'camera-defaults') return 'Save · Server update required';
		if (target === 'access') return 'Target · identity v1';
		return undefined;
	}

	function credentialState(camera: CameraSettings): string {
		return camera.username_configured && camera.password_configured
			? 'Credentials saved'
			: 'Credentials needed';
	}

	function healthClass(health: CameraSettings['health']): string {
		switch (health) {
			case 'online':
				return 'border-emerald-500/35 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300';
			case 'degraded':
			case 'stale':
				return 'border-amber-500/35 bg-amber-500/10 text-amber-700 dark:text-amber-300';
			case 'offline':
				return 'border-destructive/35 bg-destructive/10 text-destructive';
			default:
				return 'border-border bg-muted text-muted-foreground';
		}
	}

	function healthLabel(health: CameraSettings['health']): string {
		return health ?? 'pending';
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
					<MobileSettingsIndex {config} {cameras} health={serverHealth} />
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

			<div class={mobileSectionClass('camera-defaults')}>
				<MobileCameraDefaultsSection {cameras} {config} />
				<div class="hidden md:block">
					<CameraDefaultsSection {cameras} {config} />
				</div>
			</div>

			<div class={mobileSectionClass('event-sources')}>
				<EventSourcesSection health={serverHealth} healthError={serverHealthError} />
			</div>

			<div class={mobileSectionClass('groups')}>
				<GroupsSection />
			</div>

			<div class={mobileSectionClass('access')}>
				<MobileAccessSection />
				<div class="hidden md:block">
					<AccessSection />
				</div>
			</div>

			{#if mobileFocus?.id === 'camera-defaults'}
				<MobileSettingsActionBar
					action="Add an exception"
					capability="keeppeek.runtime-config.v1"
				/>
			{:else if mobileFocus?.id === 'access'}
				<MobileSettingsActionBar action="New token" capability="keeppeek.identity.v1" />
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
					onrestart={() => void restartRecorder()}
				/>
			</div>

			<Card.Root class="hidden md:grid">
				<Card.Header>
					<Card.Title>Camera setup</Card.Title>
					<Card.Action>
						<Button variant="outline" size="sm" onclick={openManualCamera}>
							<PlusIcon />
							Add camera
						</Button>
					</Card.Action>
				</Card.Header>
				<Card.Content class="space-y-6 pb-6">
					<section class="space-y-3" aria-labelledby="camera-discovery-title">
						<div class="flex items-center justify-between gap-3">
							<h2 id="camera-discovery-title" class="text-sm font-semibold">Discovery</h2>
							{#if discovered.length > 0}
								<span class="text-xs text-muted-foreground">{discovered.length} found</span>
							{/if}
						</div>
						<div class="flex flex-col gap-2 sm:flex-row sm:items-end">
							<label class="grid min-w-0 flex-1 gap-1.5 text-sm font-medium" for="subnet-prefixes">
								Subnet prefixes
								<Input
									id="subnet-prefixes"
									bind:value={subnetPrefixes}
									placeholder="192.168.0"
									autocomplete="off"
								/>
							</label>
							<Button onclick={discoverCameras} disabled={discovering}>
								<SearchIcon class={discovering ? 'animate-spin' : undefined} />
								{discovering ? 'Discovering' : 'Discover'}
							</Button>
						</div>
						{#if discoveryError}
							<p class="text-sm text-destructive" role="alert">{discoveryError}</p>
						{/if}
						{#if discovered.length > 0}
							<div class="divide-border overflow-hidden rounded-md border">
								{#each discovered as camera (camera.ip)}
									<div
										class="flex flex-col gap-3 p-3 sm:flex-row sm:items-center sm:justify-between"
									>
										<div class="min-w-0">
											<div class="flex flex-wrap items-center gap-x-2 gap-y-1">
												<p class="truncate text-sm font-medium">
													{camera.name ?? camera.model ?? camera.ip}
												</p>
												<span
													class="rounded border px-1.5 py-0.5 text-xs font-medium {healthClass(
														camera.health
													)}"
												>
													{camera.configured ? healthLabel(camera.health) : camera.brand}
												</span>
											</div>
											<p class="mt-1 truncate text-xs text-muted-foreground">
												{camera.ip}
												{#if camera.model}
													<span aria-hidden="true"> · </span>{camera.model}
												{/if}
											</p>
											{#if camera.sources.length > 0}
												<p class="mt-1 text-xs text-muted-foreground">
													{camera.sources.join(' · ')}
												</p>
											{/if}
										</div>
										<Button
											variant="outline"
											size="sm"
											onclick={() => configureDiscoveredCamera(camera)}
										>
											{camera.configured ? 'Review' : 'Configure'}
										</Button>
									</div>
								{/each}
							</div>
						{:else if didDiscover && !discovering}
							<p class="text-sm text-muted-foreground">No cameras found.</p>
						{/if}
					</section>

					<Separator />

					<section class="space-y-3" aria-labelledby="configured-cameras-title">
						<div class="flex items-center justify-between gap-3">
							<h2 id="configured-cameras-title" class="text-sm font-semibold">
								Configured cameras
							</h2>
							<span class="text-xs text-muted-foreground">{cameras.length} configured</span>
						</div>
						{#if cameras.length > 0}
							<div class="divide-border overflow-hidden rounded-md border">
								{#each cameras as camera (camera.ip)}
									<div
										class="flex flex-col gap-3 p-3 lg:flex-row lg:items-center lg:justify-between"
									>
										<div class="min-w-0">
											<div class="flex flex-wrap items-center gap-x-2 gap-y-1">
												<p class="truncate text-sm font-medium">{cameraDisplayName(camera)}</p>
												<span
													class="rounded border px-1.5 py-0.5 text-xs font-medium {healthClass(
														camera.health
													)}"
												>
													{healthLabel(camera.health)}
												</span>
											</div>
											<p class="mt-1 truncate text-xs text-muted-foreground">
												{camera.ip}
												<span aria-hidden="true"> · </span>{camera.backend} · {camera.transport}
												{#if camera.model}
													<span aria-hidden="true"> · </span>{camera.model}
												{/if}
											</p>
											<p class="mt-1 text-xs text-muted-foreground">
												{credentialState(camera)}
												{#if camera.uid_configured}<span aria-hidden="true"> · </span>P2P UID saved{/if}
											</p>
											<p class="mt-1 text-xs text-muted-foreground">
												Recording: {recordingModeLabel(camera.recording_mode)}
												{#if camera.recording_mode === 'event-boost'}
													<span aria-hidden="true"> · </span>{camera.event_recording_duration_secs}s
													main window
												{/if}
											</p>
										</div>
										<div class="flex shrink-0 flex-wrap gap-2">
											<Button
												variant="outline"
												size="sm"
												href={liveCameraHref(camera.id)}
												aria-label={`Open ${cameraDisplayName(camera)} live view`}
											>
												<RadioIcon />
												Live
											</Button>
											<Button
												variant="outline"
												size="sm"
												href={cameraInfoHref(camera.id)}
												aria-label={`Open ${cameraDisplayName(camera)} camera information`}
											>
												Details
											</Button>
											<Button variant="outline" size="sm" onclick={() => editCamera(camera)}>
												<PencilIcon />
												Edit
											</Button>
											<Button
												variant="destructive"
												size="sm"
												disabled={removingIp === camera.ip}
												onclick={() => void removeCamera(camera)}
											>
												<Trash2Icon />
												{removingIp === camera.ip ? 'Removing' : 'Remove'}
											</Button>
										</div>
									</div>
								{/each}
							</div>
						{:else}
							<p class="text-sm text-muted-foreground">No cameras configured.</p>
						{/if}
					</section>

					{#if editorMode}
						<Separator />
						<form bind:this={cameraEditor} class="space-y-4" onsubmit={saveCamera}>
							<div class="flex flex-wrap items-center justify-between gap-3">
								<h2 class="text-sm font-semibold">{isNewCamera ? 'Add camera' : 'Edit camera'}</h2>
								<Button variant="ghost" size="sm" onclick={closeEditor}>
									<XIcon />
									Cancel
								</Button>
							</div>
							<div class="grid gap-4 md:grid-cols-2">
								<label class="grid gap-1.5 text-sm font-medium" for="camera-ip">
									IP address
									<Input
										id="camera-ip"
										bind:value={form.ip}
										disabled={!isNewCamera}
										autocomplete="off"
										required
									/>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-name">
									Display name
									<Input id="camera-name" bind:value={form.displayName} autocomplete="off" />
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-username">
									Username
									<Input
										id="camera-username"
										bind:value={form.username}
										placeholder={isNewCamera ? '' : 'Enter to replace'}
										autocomplete="username"
									/>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-password">
									Password
									<Input
										id="camera-password"
										type="password"
										bind:value={form.password}
										placeholder={isNewCamera ? '' : 'Enter to replace'}
										autocomplete="new-password"
									/>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-manufacturer">
									Manufacturer override
									<Input
										id="camera-manufacturer"
										bind:value={form.manufacturer}
										autocomplete="off"
									/>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-onvif-port">
									ONVIF port
									<Input
										id="camera-onvif-port"
										bind:value={form.onvifPort}
										inputmode="numeric"
										autocomplete="off"
									/>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-http-port">
									HTTP port
									<Input
										id="camera-http-port"
										bind:value={form.httpPort}
										inputmode="numeric"
										autocomplete="off"
									/>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-main-rtsp-url">
									Main RTSP stream URL
									<Input
										id="camera-main-rtsp-url"
										bind:value={form.mainRtspUrl}
										autocomplete="off"
									/>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-sub-rtsp-url">
									Sub RTSP stream URL
									<Input id="camera-sub-rtsp-url" bind:value={form.subRtspUrl} autocomplete="off" />
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-backend">
									Backend
									<select id="camera-backend" class={selectClass} bind:value={form.backend}>
										<option value="auto">Auto</option>
										<option value="retina">Retina RTSP</option>
										<option value="reo-proto">Reo-Proto</option>
									</select>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-transport">
									Transport
									<select id="camera-transport" class={selectClass} bind:value={form.transport}>
										<option value="tcp">TCP</option>
										<option value="udp">UDP</option>
									</select>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-recording-mode">
									Recording
									<select
										id="camera-recording-mode"
										class={selectClass}
										bind:value={form.recordingMode}
									>
										<option value="event-boost">Sub, switch to main on events (recommended)</option>
										<option value="sub">Sub only</option>
										<option value="main">Main only</option>
										<option value="both">Main + sub</option>
										<option value="off">Don't record</option>
									</select>
								</label>
								{#if form.recordingMode === 'event-boost'}
									<label
										class="grid gap-1.5 text-sm font-medium"
										for="camera-event-recording-duration"
									>
										Main recording after an event (seconds)
										<Input
											id="camera-event-recording-duration"
											bind:value={form.eventRecordingDurationSeconds}
											inputmode="numeric"
											autocomplete="off"
										/>
									</label>
								{/if}
								<label
									class="flex items-start gap-3 rounded-md border border-hairline bg-raised p-3 md:col-span-2"
									for="camera-record-generic-motion-events"
								>
									<input
										id="camera-record-generic-motion-events"
										type="checkbox"
										bind:checked={form.recordGenericMotionEvents}
										class="mt-0.5 size-4 shrink-0 accent-primary"
									/>
									<span class="min-w-0">
										<span class="block text-sm font-medium">Store generic motion events</span>
										<span class="mt-1 block text-xs leading-5 text-muted-foreground">
											Off stores only person, animal, and vehicle alarms and skips generic motion
											snapshots.
										</span>
									</span>
								</label>
								<label class="grid gap-1.5 text-sm font-medium" for="camera-uid">
									P2P UID
									<Input
										id="camera-uid"
										bind:value={form.uid}
										placeholder="Optional"
										autocomplete="off"
									/>
								</label>
							</div>
							<p class="text-xs leading-5 text-muted-foreground">
								Event boost switches from sub to main at a main-stream keyframe, resets its timer on
								every event, then returns to sub at a sub-stream keyframe after the deadline.
							</p>
							{#if !isNewCamera && cameras.find((camera) => camera.ip === editorIp)?.uid_configured}
								<label class="flex items-center gap-2 text-sm">
									<input bind:checked={form.clearUid} type="checkbox" />
									Remove stored P2P UID
								</label>
							{/if}
							{#if editorError}
								<p class="text-sm text-destructive" role="alert">{editorError}</p>
							{/if}
							<div class="flex justify-end gap-2">
								<Button variant="outline" onclick={closeEditor}>Cancel</Button>
								<Button type="submit" disabled={saving}>
									{#if !saving}<SaveIcon />{:else}<RefreshCwIcon class="animate-spin" />{/if}
									{saving ? 'Saving' : 'Save camera'}
								</Button>
							</div>
						</form>
					{/if}
				</Card.Content>
			</Card.Root>

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
