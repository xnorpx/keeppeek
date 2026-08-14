<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount, tick } from 'svelte';
	import {
		discoverSettingsCameras,
		getConfig,
		getHealth,
		getHealthAt,
		getSettingsCameras,
		removeSettingsCamera,
		restartSettingsServer,
		updateSettingsConfig,
		updateSettingsCamera
	} from '$lib/api';
	import type {
		CameraBackend,
		CameraSettings,
		CameraSettingsUpdate,
		CameraTransport,
		DiscoveredCameraSettings,
		SanitizedConfig,
		SettingsConfigUpdate
	} from '$lib/types';
	import * as Card from '$lib/components/ui/card/index.js';
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

	type EditorMode = 'new' | 'edit' | null;

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
	};

	type RuntimeSettingsForm = {
		host: string;
		port: string;
		mediumTermPath: string;
		longTermPath: string;
		recordingCatalogPath: string;
		eventThumbnailPath: string;
		eventThumbnailMaxMegabytes: string;
		moveExistingRecordings: boolean;
		shortTermSeconds: string;
		mediumTermSeconds: string;
		flushIntervalSeconds: string;
		writeBufferBytes: string;
		longTermMaxGigabytes: string;
	};

	const selectClass =
		'border-input bg-background ring-offset-background focus-visible:border-ring focus-visible:ring-ring/50 h-9 w-full rounded-md border px-3 text-sm font-medium shadow-xs outline-none focus-visible:ring-[3px]';
	const MAX_WRITE_BUFFER_BYTES = 64 * 1024 * 1024;
	const DEFAULT_REOLINK_ONVIF_PORT = 8000;
	const DEFAULT_REOLINK_HTTP_PORT = 80;

	let config = $state.raw<SanitizedConfig | null>(null);
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
	let editorMode = $state<EditorMode>(null);
	let editorIp = $state<string | null>(null);
	let form = $state<CameraForm>(emptyForm());
	let cameraEditor = $state<HTMLFormElement | null>(null);
	let editingRuntimeSettings = $state(false);
	let savingRuntimeSettings = $state(false);
	let runtimeSettingsError = $state<string | null>(null);
	let runtimeSettingsForm = $state<RuntimeSettingsForm>(emptyRuntimeSettingsForm());
	let restartTargetOrigin = $state<string | null>(null);
	let isNewCamera = $derived(editorMode === 'new');

	onMount(() => {
		void loadSettings();
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
			transport: 'tcp'
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
			transport: camera.transport
		};
	}

	function emptyRuntimeSettingsForm(): RuntimeSettingsForm {
		return {
			host: '',
			port: '',
			mediumTermPath: '',
			longTermPath: '',
			recordingCatalogPath: '',
			eventThumbnailPath: '',
			eventThumbnailMaxMegabytes: '',
			moveExistingRecordings: false,
			shortTermSeconds: '',
			mediumTermSeconds: '',
			flushIntervalSeconds: '',
			writeBufferBytes: '',
			longTermMaxGigabytes: ''
		};
	}

	function runtimeSettingsFormFromConfig(config: SanitizedConfig): RuntimeSettingsForm {
		return {
			host: config.host,
			port: config.port.toString(),
			mediumTermPath: config.storage.medium_term_path,
			longTermPath: config.storage.long_term_path,
			recordingCatalogPath: config.storage.recording_catalog_path,
			eventThumbnailPath: config.storage.event_thumbnail_path,
			eventThumbnailMaxMegabytes: config.storage.event_thumbnail_max_mb.toString(),
			moveExistingRecordings: false,
			shortTermSeconds: config.storage.short_term_secs.toString(),
			mediumTermSeconds: config.storage.medium_term_secs.toString(),
			flushIntervalSeconds: config.storage.flush_interval_secs.toString(),
			writeBufferBytes: config.storage.write_buffer_bytes.toString(),
			longTermMaxGigabytes: config.storage.long_term_max_gb.toString()
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
			const [nextConfig, nextCameras] = await Promise.all([getConfig(), getSettingsCameras()]);
			config = nextConfig;
			cameras = nextCameras;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to load settings.';
		} finally {
			loading = false;
		}
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
			discovered = await discoverSettingsCameras(parseSubnetPrefixes());
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

	function openRuntimeSettings() {
		if (!config) return;
		runtimeSettingsForm = runtimeSettingsFormFromConfig(config);
		runtimeSettingsError = null;
		editingRuntimeSettings = true;
	}

	function closeRuntimeSettings() {
		editingRuntimeSettings = false;
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
		const host = runtimeSettingsForm.host.trim();
		if (!host || /\s/.test(host)) {
			throw new Error('Host must be a nonempty address or hostname.');
		}
		const mediumTermPath = runtimeSettingsForm.mediumTermPath.trim();
		const longTermPath = runtimeSettingsForm.longTermPath.trim();
		if (!mediumTermPath || mediumTermPath.includes('\0')) {
			throw new Error('Medium-term storage path must be nonempty.');
		}
		if (!longTermPath || longTermPath.includes('\0')) {
			throw new Error('Long-term storage path must be nonempty.');
		}
		const recordingCatalogPath = runtimeSettingsForm.recordingCatalogPath.trim();
		if (!recordingCatalogPath || recordingCatalogPath.includes('\0')) {
			throw new Error('Recording metadata database path must be nonempty.');
		}
		const eventThumbnailPath = runtimeSettingsForm.eventThumbnailPath.trim();
		if (!eventThumbnailPath || eventThumbnailPath.includes('\0')) {
			throw new Error('Event JPEG storage path must be nonempty.');
		}
		return {
			host,
			port: parseWholeNumber(runtimeSettingsForm.port, 'Server port', 1, 65_535),
			move_existing_recordings: runtimeSettingsForm.moveExistingRecordings,
			storage: {
				medium_term_path: mediumTermPath,
				long_term_path: longTermPath,
				recording_catalog_path: recordingCatalogPath,
				event_thumbnail_path: eventThumbnailPath,
				event_thumbnail_max_mb: parseWholeNumber(
					runtimeSettingsForm.eventThumbnailMaxMegabytes,
					'Event JPEG limit',
					0,
					Number.MAX_SAFE_INTEGER
				),
				short_term_secs: parseWholeNumber(
					runtimeSettingsForm.shortTermSeconds,
					'Short-term buffer',
					0,
					Number.MAX_SAFE_INTEGER
				),
				medium_term_secs: parseWholeNumber(
					runtimeSettingsForm.mediumTermSeconds,
					'Medium-term segment',
					0,
					Number.MAX_SAFE_INTEGER
				),
				flush_interval_secs: parseWholeNumber(
					runtimeSettingsForm.flushIntervalSeconds,
					'Flush interval',
					0,
					Number.MAX_SAFE_INTEGER
				),
				write_buffer_bytes: parseWholeNumber(
					runtimeSettingsForm.writeBufferBytes,
					'Write buffer',
					1,
					MAX_WRITE_BUFFER_BYTES
				),
				long_term_max_gb: parseWholeNumber(
					runtimeSettingsForm.longTermMaxGigabytes,
					'Long-term maximum',
					0,
					Number.MAX_SAFE_INTEGER
				)
			}
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
			transport: form.transport
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
			const result = await updateSettingsCamera(ip, update);
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
			await removeSettingsCamera(camera.ip);
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

		savingRuntimeSettings = true;
		runtimeSettingsError = null;
		try {
			const result = await updateSettingsConfig(update);
			config = result.config;
			restartTargetOrigin = restartOrigin(result.config);
			pendingRestart ||= result.restart_required;
			statusMessage = 'Server and storage settings saved.';
			closeRuntimeSettings();
		} catch (cause) {
			runtimeSettingsError =
				cause instanceof Error ? cause.message : 'Server and storage settings were not saved.';
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
				if (targetOrigin === window.location.origin) {
					await getHealth();
					window.location.reload();
				} else {
					await getHealthAt(targetOrigin);
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
			await restartSettingsServer();
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

	function liveCameraHref(cameraId: string): string {
		return `${resolve('/')}?camera=${encodeURIComponent(cameraId)}`;
	}

	function cameraInfoHref(cameraId: string): string {
		return `${resolve('/camera')}?camera=${encodeURIComponent(cameraId)}`;
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

	function formatBytes(bytes: number): string {
		if (!Number.isFinite(bytes) || bytes < 0) return 'Unavailable';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		let value = bytes;
		let index = 0;
		while (value >= 1_024 && index < units.length - 1) {
			value /= 1_024;
			index += 1;
		}
		const digits = value >= 10 || index === 0 ? 0 : 1;
		return `${value.toFixed(digits)} ${units[index]}`;
	}

	function formatBitrate(bitsPerSecond: number): string {
		return `${formatBytes(bitsPerSecond / 8)}/s`;
	}

	function formatRetentionDays(days: number): string {
		if (days < 1) return `${Math.max(1, Math.round(days * 24))}h`;
		return days < 10 ? `${days.toFixed(1)} days` : `${Math.round(days)} days`;
	}
</script>

<svelte:head>
	<title>Settings - KeepPeek</title>
</svelte:head>

<div class="space-y-6">
	<h1 class="text-2xl font-bold tracking-tight">Settings</h1>

	{#if loading}
		<div class="max-w-5xl space-y-6">
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
		<div class="max-w-5xl space-y-6">
			{#if pendingRestart || statusMessage || restartError}
				<div
					class="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border bg-muted/45 px-3 py-2 text-sm"
					role="status"
				>
					<p class={restartError ? 'text-destructive' : 'text-muted-foreground'}>
						{restartError ?? statusMessage ?? 'Saved changes are ready to apply.'}
					</p>
					{#if pendingRestart}
						<Button size="sm" onclick={applyChanges} disabled={restarting}>
							<RotateCcwIcon class={restarting ? 'animate-spin' : undefined} />
							{restarting ? 'Applying changes' : 'Apply changes'}
						</Button>
					{/if}
				</div>
			{/if}

			<Card.Root>
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

			<Card.Root>
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

			<form class="space-y-4" onsubmit={saveRuntimeSettings}>
				<div class="grid gap-6 lg:grid-cols-2">
					<Card.Root>
						<Card.Header>
							<Card.Title>Server</Card.Title>
							{#if !editingRuntimeSettings}
								<Card.Action>
									<Button variant="outline" size="sm" onclick={openRuntimeSettings}>
										<PencilIcon />
										Edit server
									</Button>
								</Card.Action>
							{/if}
						</Card.Header>
						<Card.Content class="pb-6">
							{#if editingRuntimeSettings}
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

					<Card.Root>
						<Card.Header>
							<Card.Title>Storage</Card.Title>
							{#if !editingRuntimeSettings}
								<Card.Action>
									<Button variant="outline" size="sm" onclick={openRuntimeSettings}>
										<PencilIcon />
										Edit storage
									</Button>
								</Card.Action>
							{/if}
						</Card.Header>
						<Card.Content class="pb-6">
							{#if editingRuntimeSettings}
								<div class="grid gap-4 sm:grid-cols-2">
									<label
										class="grid gap-1.5 text-sm font-medium sm:col-span-2"
										for="medium-term-path"
									>
										Medium-term path
										<Input
											id="medium-term-path"
											bind:value={runtimeSettingsForm.mediumTermPath}
											autocomplete="off"
										/>
									</label>
									<label
										class="grid gap-1.5 text-sm font-medium sm:col-span-2"
										for="long-term-path"
									>
										Long-term path
										<Input
											id="long-term-path"
											bind:value={runtimeSettingsForm.longTermPath}
											autocomplete="off"
										/>
									</label>
									<label
										class="grid gap-1.5 text-sm font-medium sm:col-span-2"
										for="recording-catalog-path"
									>
										Recording metadata database path
										<Input
											id="recording-catalog-path"
											bind:value={runtimeSettingsForm.recordingCatalogPath}
											autocomplete="off"
										/>
									</label>
									<label
										class="grid gap-1.5 text-sm font-medium sm:col-span-2"
										for="event-thumbnail-path"
									>
										Event JPEG storage path
										<Input
											id="event-thumbnail-path"
											bind:value={runtimeSettingsForm.eventThumbnailPath}
											autocomplete="off"
										/>
									</label>
									<label
										class="grid gap-1.5 text-sm font-medium"
										for="event-thumbnail-max-megabytes"
									>
										Event JPEG limit MB
										<Input
											id="event-thumbnail-max-megabytes"
											bind:value={runtimeSettingsForm.eventThumbnailMaxMegabytes}
											inputmode="numeric"
											autocomplete="off"
										/>
									</label>
									<label
										class="flex items-center gap-2 text-sm font-medium sm:col-span-2"
										for="move-existing-recordings"
									>
										<input
											id="move-existing-recordings"
											type="checkbox"
											bind:checked={runtimeSettingsForm.moveExistingRecordings}
										/>
										Move current storage files
									</label>
									<label class="grid gap-1.5 text-sm font-medium" for="short-term-seconds">
										Short-term buffer seconds
										<Input
											id="short-term-seconds"
											bind:value={runtimeSettingsForm.shortTermSeconds}
											inputmode="numeric"
											autocomplete="off"
										/>
									</label>
									<label class="grid gap-1.5 text-sm font-medium" for="medium-term-seconds">
										Medium-term segment seconds
										<Input
											id="medium-term-seconds"
											bind:value={runtimeSettingsForm.mediumTermSeconds}
											inputmode="numeric"
											autocomplete="off"
										/>
									</label>
									<label class="grid gap-1.5 text-sm font-medium" for="flush-interval-seconds">
										Flush interval seconds
										<Input
											id="flush-interval-seconds"
											bind:value={runtimeSettingsForm.flushIntervalSeconds}
											inputmode="numeric"
											autocomplete="off"
										/>
									</label>
									<label class="grid gap-1.5 text-sm font-medium" for="write-buffer-bytes">
										Write buffer bytes
										<Input
											id="write-buffer-bytes"
											bind:value={runtimeSettingsForm.writeBufferBytes}
											inputmode="numeric"
											autocomplete="off"
										/>
									</label>
									<label class="grid gap-1.5 text-sm font-medium" for="long-term-max-gigabytes">
										Long-term max GB
										<Input
											id="long-term-max-gigabytes"
											bind:value={runtimeSettingsForm.longTermMaxGigabytes}
											inputmode="numeric"
											autocomplete="off"
										/>
									</label>
								</div>
							{:else}
								<div class="space-y-4">
									<dl class="space-y-3 text-sm">
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Medium-term path</dt>
											<dd class="max-w-[60%] text-right break-all">
												{config.storage.medium_term_path}
											</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Long-term path</dt>
											<dd class="max-w-[60%] text-right break-all">
												{config.storage.long_term_path}
											</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Recording metadata database</dt>
											<dd class="max-w-[60%] text-right break-all">
												{config.storage.recording_catalog_path}
											</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Event JPEG storage</dt>
											<dd class="max-w-[60%] text-right break-all">
												{config.storage.event_thumbnail_path}
											</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Event JPEG limit</dt>
											<dd>
												{config.storage.event_thumbnail_max_mb === 0
													? 'Unlimited'
													: `${config.storage.event_thumbnail_max_mb} MB`}
											</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Short-term buffer</dt>
											<dd>{config.storage.short_term_secs}s</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Medium-term segment</dt>
											<dd>{config.storage.medium_term_secs}s</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Flush interval</dt>
											<dd>{config.storage.flush_interval_secs}s</dd>
										</div>
										<Separator />
										<div class="flex justify-between gap-4">
											<dt class="text-muted-foreground">Long-term max</dt>
											<dd>
												{config.storage.long_term_max_gb === 0
													? 'Unlimited'
													: `${config.storage.long_term_max_gb} GB`}
											</dd>
										</div>
									</dl>
									<Separator />
									<section class="space-y-3" aria-labelledby="recording-estimate-title">
										<div class="flex flex-wrap items-center justify-between gap-2">
											<h2 id="recording-estimate-title" class="text-sm font-semibold">
												Current recording estimate
											</h2>
											<span class="text-xs text-muted-foreground">
												{config.recording_estimate.known_streams} known of {config
													.recording_estimate.known_streams +
													config.recording_estimate.unknown_streams} streams
											</span>
										</div>
										<dl class="grid grid-cols-2 gap-x-4 gap-y-3 text-sm">
											<div class="min-w-0">
												<dt class="text-muted-foreground">Estimated rate</dt>
												<dd class="mt-1 font-mono">
													{formatBitrate(config.recording_estimate.estimated_bitrate_bps)}
												</dd>
											</div>
											<div class="min-w-0">
												<dt class="text-muted-foreground">1 day</dt>
												<dd class="mt-1 font-mono">
													{formatBytes(config.recording_estimate.bytes_per_day)}
												</dd>
											</div>
											<div class="min-w-0">
												<dt class="text-muted-foreground">7 days</dt>
												<dd class="mt-1 font-mono">
													{formatBytes(config.recording_estimate.bytes_per_day * 7)}
												</dd>
											</div>
											<div class="min-w-0">
												<dt class="text-muted-foreground">30 days</dt>
												<dd class="mt-1 font-mono">
													{formatBytes(config.recording_estimate.bytes_per_day * 30)}
												</dd>
											</div>
											<div class="min-w-0">
												<dt class="text-muted-foreground">At long-term cap</dt>
												<dd class="mt-1 font-mono">
													{config.storage.long_term_max_gb === 0
														? 'Unlimited'
														: config.recording_estimate.estimated_retention_days === null
															? 'Unavailable'
															: formatRetentionDays(
																	config.recording_estimate.estimated_retention_days
																)}
												</dd>
											</div>
										</dl>
									</section>
								</div>
							{/if}
						</Card.Content>
					</Card.Root>
				</div>
				{#if editingRuntimeSettings}
					{#if runtimeSettingsError}
						<p class="text-sm text-destructive" role="alert">{runtimeSettingsError}</p>
					{/if}
					<div class="flex justify-end gap-2">
						<Button variant="outline" onclick={closeRuntimeSettings}>Cancel</Button>
						<Button type="submit" disabled={savingRuntimeSettings}>
							{#if !savingRuntimeSettings}<SaveIcon />{:else}<RefreshCwIcon
									class="animate-spin"
								/>{/if}
							{savingRuntimeSettings ? 'Saving' : 'Save settings'}
						</Button>
					</div>
				{/if}
			</form>
		</div>
	{/if}
</div>
