<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import {
		applyCatalogCameraDefaults,
		applyCatalogStreamHints,
		applyManualCameraAddress,
		cameraStreamVerificationError,
		cameraWizardSteps,
		cameraWizardUpdate,
		draftFromDiscoveredCamera,
		emptyCameraWizardDraft,
		exactCatalogCameraMatch,
		manualCameraAddressError,
		validateCameraWizardStep,
		type CameraWizardDraft,
		type CameraWizardStep
	} from '$lib/camera-wizard';
	import type {
		CameraCatalogCamera,
		CameraCatalogInfo,
		CameraDiscoveryNetwork,
		CameraSettingsUpdateResponse,
		CameraStreamProbeResult,
		DiscoveredCameraSettings
	} from '$lib/types';
	import CameraCatalogEvidence from '$lib/components/CameraCatalogEvidence.svelte';
	import CameraOnboardingEvidence from '$lib/components/CameraOnboardingEvidence.svelte';
	import DiscoveryProgressState from '$lib/components/DiscoveryProgressState.svelte';
	import DesktopCameraWizardStreamsStep from '$lib/components/DesktopCameraWizardStreamsStep.svelte';
	import MobileAddCameraWizard, {
		type MobileCameraWizardStage
	} from '$lib/components/MobileAddCameraWizard.svelte';
	import ArrowLeftIcon from '@lucide/svelte/icons/arrow-left';
	import ArrowRightIcon from '@lucide/svelte/icons/arrow-right';
	import CheckIcon from '@lucide/svelte/icons/check';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import LockIcon from '@lucide/svelte/icons/lock';
	import SearchIcon from '@lucide/svelte/icons/search';

	const stepLabels: Record<CameraWizardStep, string> = {
		find: 'Find & connect',
		connect: 'Connection options',
		streams: 'Streams',
		recording: 'Recording',
		review: 'Review & save'
	};

	let stepIndex = $state(0);
	let draft = $state.raw<CameraWizardDraft>(emptyCameraWizardDraft());
	let discovered = $state.raw<DiscoveredCameraSettings[]>([]);
	let selectedCatalogCamera = $state.raw<CameraCatalogCamera | null>(null);
	let catalogInfo = $state.raw<CameraCatalogInfo | null>(null);
	let catalogQuery = $state('');
	let catalogResults = $state.raw<CameraCatalogCamera[]>([]);
	let catalogSearching = $state(false);
	let catalogSearchAttempted = $state(false);
	let subnetPrefixes = $state('192.168.1');
	let discoveryNetworks = $state.raw<CameraDiscoveryNetwork[]>([]);
	let subnetScopeEdited = $state(false);
	let manualAddress = $state('');
	let discovering = $state(false);
	let discoveryController = $state.raw<AbortController | null>(null);
	let discoveryCancelled = $state(false);
	let discoveryAttempted = $state(false);
	let discoveryStartedAt = $state(0);
	let discoveryElapsedMs = $state(0);
	let discoverySubnetCount = $state(0);
	let streamResolution = $state<'unresolved' | 'catalog' | 'probing' | 'onvif' | 'manual'>(
		'unresolved'
	);
	let streamProbeMessage = $state<string | null>(null);
	let streamProbeResult = $state.raw<CameraStreamProbeResult | null>(null);
	let streamProbing = $state(false);
	let streamProbeRevision = $state(0);
	let lastStreamProbeRevision = $state(-1);
	let saving = $state(false);
	let error = $state<string | null>(null);
	let saved = $state.raw<CameraSettingsUpdateResponse | null>(null);
	let mobileViewport = $state(false);
	const controlClient = useControlClient();
	let currentStep = $derived(cameraWizardSteps[stepIndex]);
	let manualAddressError = $derived(manualCameraAddressError(manualAddress));
	let manualAddressValid = $derived(manualAddress.trim().length > 0 && manualAddressError === null);
	let manualRtspAddress = $derived(manualAddress.trim().toLocaleLowerCase().startsWith('rtsp://'));
	let firstConnectionReady = $derived(
		validateCameraWizardStep('find', draft) === null &&
			validateCameraWizardStep('connect', draft) === null
	);
	let streamVerificationError = $derived(cameraStreamVerificationError(draft, streamProbeResult));
	let selectedCatalogFromDiscovery = $derived.by(() => {
		const selectedId = selectedCatalogCamera?.id;
		return Boolean(
			selectedId &&
			discovered.some((camera) => camera.ip === draft.ip && camera.catalog?.id === selectedId)
		);
	});
	let credentialReview = $derived(
		draft.username || draft.password
			? 'Provided · password write-only'
			: draft.defaultUsernameConfigured && draft.defaultPasswordConfigured
				? 'Configured default · values not exposed'
				: 'Missing'
	);
	let firstScreenProbeStatus = $derived.by(() => {
		if (manualRtspAddress) return 'Manual RTSP URL supplied. ONVIF lookup is skipped.';
		if (streamProbing) {
			return draft.onvifPort.trim()
				? `Trying ONVIF at ${draft.ip}:${draft.onvifPort}…`
				: `Trying ONVIF at ${draft.ip} on common ports…`;
		}
		if (streamResolution === 'onvif') {
			return draft.onvifPort.trim()
				? `ONVIF stream endpoints are ready on port ${draft.onvifPort}.`
				: 'ONVIF stream endpoints are ready.';
		}
		if (firstConnectionReady) return streamProbeMessage ?? 'ONVIF lookup will start automatically.';
		return 'Enter a valid address, username, and password to start ONVIF automatically.';
	});
	let catalogStreamsApplied = $derived.by(() => {
		const hints = selectedCatalogCamera?.stream_hints;
		if (!hints || (!hints.main_rtsp_url && !hints.sub_rtsp_url)) return false;
		return (
			(hints.main_rtsp_url === null || draft.mainRtspUrl === hints.main_rtsp_url) &&
			(hints.sub_rtsp_url === null || draft.subRtspUrl === hints.sub_rtsp_url)
		);
	});
	let mobileStage = $derived<MobileCameraWizardStage>(
		currentStep === 'streams'
			? 'streams'
			: currentStep === 'recording' || currentStep === 'review'
				? 'review'
				: 'find-connect'
	);

	onMount(() => {
		const media = window.matchMedia('(max-width: 767px)');
		const updateViewport = () => (mobileViewport = media.matches);
		updateViewport();
		media.addEventListener('change', updateViewport);
		void loadOnboardingDefaults();
		return () => media.removeEventListener('change', updateViewport);
	});

	async function loadOnboardingDefaults(): Promise<void> {
		try {
			const defaults = await controlClient.getCameraOnboardingDefaults();
			discoveryNetworks = defaults.networks;
			draft = {
				...draft,
				defaultUsernameConfigured: defaults.username_configured,
				defaultPasswordConfigured: defaults.password_configured
			};
			if (!subnetScopeEdited) {
				const preferred = defaults.networks.find((network) => network.preferred);
				const first = preferred ?? defaults.networks[0];
				if (first) subnetPrefixes = subnetPrefix(first.cidr);
			}
		} catch {
			// Manual onboarding remains available when bootstrap evidence is unavailable.
		}
	}

	function subnetPrefix(cidr: string): string {
		return cidr.endsWith('.0/24') ? cidr.slice(0, -5) : cidr;
	}

	function setSubnetPrefixes(value: string): void {
		subnetScopeEdited = true;
		subnetPrefixes = value;
	}

	function toggleDiscoveryNetwork(network: CameraDiscoveryNetwork): void {
		const prefix = subnetPrefix(network.cidr);
		const selected = subnetPrefixes
			.split(',')
			.map((value) => value.trim())
			.filter(Boolean);
		const next = new Set(selected);
		if (next.has(prefix) && next.size > 1) next.delete(prefix);
		else next.add(prefix);
		setSubnetPrefixes([...next].join(', '));
	}

	$effect(() => {
		if (!discovering) return;
		const updateElapsed = () => {
			discoveryElapsedMs = Math.max(0, performance.now() - discoveryStartedAt);
		};
		updateElapsed();
		const timer = window.setInterval(updateElapsed, 100);
		return () => window.clearInterval(timer);
	});

	$effect(() => {
		const nextDraft = draft;
		const revision = streamProbeRevision;
		if (
			(currentStep !== 'find' && currentStep !== 'connect') ||
			!canProbeCameraStreams(nextDraft) ||
			streamProbing ||
			lastStreamProbeRevision === revision
		) {
			return;
		}
		const timer = window.setTimeout(() => {
			void probeCameraStreams(nextDraft, revision);
		}, 350);
		return () => window.clearTimeout(timer);
	});

	function updateDraft(update: Partial<CameraWizardDraft>): void {
		draft = { ...draft, ...update };
		if (
			update.ip !== undefined ||
			update.username !== undefined ||
			update.password !== undefined ||
			update.onvifPort !== undefined
		) {
			streamProbeRevision += 1;
			streamProbeResult = null;
		}
		if (update.mainRtspUrl !== undefined || update.subRtspUrl !== undefined) {
			streamProbeRevision += 1;
			streamProbeResult = null;
			streamResolution = 'manual';
			streamProbeMessage = null;
		}
		error = null;
	}

	function parseSubnetPrefixes(): string[] {
		const values = subnetPrefixes
			.split(',')
			.map((value) => value.trim())
			.filter(Boolean);
		const networks = values.map((value) => {
			const prefix = value.endsWith('.0/24') ? value.slice(0, -5) : value.replace(/\.$/, '');
			const octets = prefix.split('.');
			if (
				octets.length !== 3 ||
				octets.some((octet) => !/^\d+$/.test(octet) || Number(octet) > 255)
			) {
				throw new Error('Enter subnet prefixes such as 192.168.1.');
			}
			return `${octets.join('.')}.0/24`;
		});
		return [...new Set(networks)];
	}

	async function discover(): Promise<void> {
		if (discovering) return;
		let networks: string[];
		try {
			networks = parseSubnetPrefixes();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Camera discovery settings are invalid.';
			return;
		}
		discovered = [];
		discoverySubnetCount = networks.length;
		discoveryElapsedMs = 0;
		discoveryStartedAt = performance.now();
		discoveryCancelled = false;
		discovering = true;
		error = null;
		const controller = new AbortController();
		discoveryController = controller;
		try {
			const [cameras, nextCatalogInfo] = await Promise.all([
				controlClient.discoverCameras(networks, {
					signal: controller.signal,
					onProgress: (cameras) => {
						if (discoveryController !== controller) return;
						discovered = cameras;
						discoveryAttempted = true;
					}
				}),
				controlClient.getCameraCatalog().catch(() => null)
			]);
			discovered = cameras;
			catalogInfo = nextCatalogInfo;
			discoveryAttempted = true;
		} catch (cause) {
			if (controller.signal.aborted) {
				discoveryCancelled = true;
				discoveryAttempted = true;
			} else {
				error = cause instanceof Error ? cause.message : 'Camera discovery failed.';
			}
		} finally {
			if (discoveryController === controller) {
				discoveryController = null;
				discovering = false;
			}
		}
	}

	function cancelDiscovery(): void {
		discoveryController?.abort();
	}

	function selectDiscovered(camera: DiscoveredCameraSettings): void {
		if (camera.configured) return;
		const selected = draftFromDiscoveredCamera(camera);
		draft = {
			...selected,
			defaultUsernameConfigured: draft.defaultUsernameConfigured,
			defaultPasswordConfigured: draft.defaultPasswordConfigured
		};
		manualAddress = '';
		selectedCatalogCamera = camera.catalog ?? null;
		streamProbeResult = null;
		streamProbeRevision += 1;
		streamResolution =
			camera.catalog?.stream_hints?.main_rtsp_url || camera.catalog?.stream_hints?.sub_rtsp_url
				? 'catalog'
				: 'unresolved';
		streamProbeMessage = null;
		error = null;
	}

	function updateManualAddress(value: string): void {
		manualAddress = value;
		const addressError = manualCameraAddressError(value);
		if (addressError !== null || !value.trim()) {
			draft = {
				...draft,
				ip: '',
				mainRtspUrl: '',
				subRtspUrl: '',
				discoveryEvidence: null
			};
			selectedCatalogCamera = null;
			streamProbeResult = null;
			streamProbeRevision += 1;
			streamResolution = 'unresolved';
			streamProbeMessage = null;
			if (error) error = null;
			return;
		}

		try {
			let nextDraft = applyManualCameraAddress(draft, value);
			const addressChanged = nextDraft.ip !== draft.ip;
			const isRtspAddress = value.trim().toLocaleLowerCase().startsWith('rtsp://');
			if (addressChanged && !isRtspAddress) {
				nextDraft = { ...nextDraft, mainRtspUrl: '', subRtspUrl: '' };
			}
			draft = nextDraft;
			if (addressChanged || isRtspAddress) selectedCatalogCamera = null;
			streamProbeResult = null;
			streamProbeRevision += 1;
			streamResolution = isRtspAddress ? 'manual' : 'unresolved';
			streamProbeMessage = null;
			error = null;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Camera address is invalid.';
		}
	}

	function updateCatalogQuery(value: string): void {
		if (catalogQuery === value) return;
		catalogQuery = value;
		catalogResults = [];
		catalogSearchAttempted = false;
		if (error) error = null;
	}

	function catalogSearchIp(): string | undefined {
		if (!manualAddress.trim()) return draft.ip.trim() || undefined;
		const nextDraft = applyManualCameraAddress(draft, manualAddress);
		if (nextDraft.ip !== draft.ip) selectedCatalogCamera = null;
		draft = nextDraft;
		return nextDraft.ip;
	}

	async function searchCatalog(): Promise<void> {
		const query = catalogQuery.trim();
		if (!query || catalogSearching) return;
		let ip: string | undefined;
		try {
			ip = catalogSearchIp();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Camera address is invalid.';
			return;
		}
		catalogSearching = true;
		catalogSearchAttempted = true;
		catalogResults = [];
		error = null;
		try {
			const [results, nextCatalogInfo] = await Promise.all([
				controlClient.searchCameraCatalog(query, { ip }),
				catalogInfo
					? Promise.resolve(catalogInfo)
					: controlClient.getCameraCatalog().catch(() => null)
			]);
			catalogResults = results;
			catalogInfo = nextCatalogInfo;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Camera catalog search failed.';
		} finally {
			catalogSearching = false;
		}
	}

	async function matchCatalogFromOnvif(
		probe: CameraStreamProbeResult,
		ip: string,
		revision: number
	): Promise<void> {
		const model = probe.model?.trim();
		if (!model || selectedCatalogCamera) return;
		try {
			const [results, nextCatalogInfo] = await Promise.all([
				controlClient.searchCameraCatalog(model, { ip }),
				catalogInfo
					? Promise.resolve(catalogInfo)
					: controlClient.getCameraCatalog().catch(() => null)
			]);
			if (revision !== streamProbeRevision || selectedCatalogCamera) return;
			catalogQuery = model;
			catalogResults = results;
			catalogSearchAttempted = true;
			catalogInfo = nextCatalogInfo;
			selectedCatalogCamera = exactCatalogCameraMatch(results, probe.manufacturer, model);
			if (selectedCatalogCamera) {
				draft = applyCatalogCameraDefaults(draft, selectedCatalogCamera);
			}
		} catch {
			// Catalog context is optional; authenticated ONVIF and media proof remain usable.
		}
	}

	function selectCatalogCamera(camera: CameraCatalogCamera): void {
		selectedCatalogCamera = camera;
		streamProbeResult = null;
		draft = applyCatalogCameraDefaults(draft, camera);
		if (camera.stream_hints) {
			draft = applyCatalogStreamHints(draft, camera.stream_hints);
			streamResolution =
				camera.stream_hints.main_rtsp_url || camera.stream_hints.sub_rtsp_url
					? 'catalog'
					: 'unresolved';
		} else {
			streamResolution = 'unresolved';
		}
		streamProbeRevision += 1;
		streamProbeMessage = null;
		error = null;
	}

	function applyCatalogStreams(): void {
		const hints = selectedCatalogCamera?.stream_hints;
		if (!hints) return;
		draft = applyCatalogStreamHints(draft, hints);
		streamProbeResult = null;
		streamResolution = 'catalog';
		streamProbeMessage = null;
		error = null;
	}

	function canProbeCameraStreams(nextDraft: CameraWizardDraft): boolean {
		return (
			validateCameraWizardStep('find', nextDraft) === null &&
			validateCameraWizardStep('connect', nextDraft) === null
		);
	}

	async function probeCameraStreams(
		nextDraft: CameraWizardDraft,
		revision = streamProbeRevision,
		force = false
	): Promise<void> {
		if (streamProbing || (!force && lastStreamProbeRevision === revision)) return;
		if (!canProbeCameraStreams(nextDraft)) return;
		lastStreamProbeRevision = revision;
		streamProbing = true;
		streamResolution = 'probing';
		streamProbeMessage = null;
		try {
			const streams = await controlClient.probeCameraStreams({
				ip: nextDraft.ip,
				username: nextDraft.username,
				password: nextDraft.password,
				onvif_port: nextDraft.onvifPort.trim() ? Number(nextDraft.onvifPort) : null,
				main_rtsp_url: nextDraft.mainRtspUrl.trim() || null,
				sub_rtsp_url: nextDraft.subRtspUrl.trim() || null,
				transport: nextDraft.transport,
				query_onvif: !manualRtspAddress
			});
			if (revision !== streamProbeRevision) return;
			const foundStream = Boolean(streams.main_rtsp_url || streams.sub_rtsp_url);
			draft = {
				...nextDraft,
				onvifPort: streams.onvif_port?.toString() ?? nextDraft.onvifPort,
				mainRtspUrl: streams.main_rtsp_url ?? nextDraft.mainRtspUrl,
				subRtspUrl: streams.sub_rtsp_url ?? nextDraft.subRtspUrl
			};
			streamProbeResult = streams;
			void matchCatalogFromOnvif(streams, nextDraft.ip, revision);
			streamResolution = manualRtspAddress
				? 'manual'
				: streams.onvif_port !== null
					? 'onvif'
					: foundStream
						? 'catalog'
						: selectedCatalogCamera?.stream_hints?.main_rtsp_url ||
							  selectedCatalogCamera?.stream_hints?.sub_rtsp_url
							? 'catalog'
							: 'unresolved';
			streamProbeMessage =
				streams.onvif_error ??
				(foundStream
					? null
					: `ONVIF responded${streams.onvif_port === null ? '' : ` on port ${streams.onvif_port}`} but did not report RTSP stream endpoints. You can enter them manually.`);
		} catch (cause) {
			if (revision !== streamProbeRevision) return;
			draft = nextDraft;
			streamProbeResult = null;
			streamResolution =
				selectedCatalogCamera?.stream_hints?.main_rtsp_url ||
				selectedCatalogCamera?.stream_hints?.sub_rtsp_url
					? 'catalog'
					: 'unresolved';
			const detail = cause instanceof Error ? cause.message : '';
			streamProbeMessage = detail.includes('another camera operation')
				? 'Another camera operation is in progress. ONVIF will retry when you edit the connection details.'
				: 'ONVIF did not respond on the automatic port search. Choose an ONVIF port in Connection options to retry, or enter RTSP URLs manually.';
		} finally {
			streamProbing = false;
		}
	}

	function tryCameraConnection(): void {
		void probeCameraStreams(draft, streamProbeRevision, true);
	}

	function next(): void {
		const validationError =
			currentStep === 'find'
				? (validateCameraWizardStep('find', draft) ?? validateCameraWizardStep('connect', draft))
				: (validateCameraWizardStep(currentStep, draft) ??
					(currentStep === 'streams' ? streamVerificationError : null));
		if (validationError) {
			error = validationError;
			return;
		}
		if ((currentStep === 'find' || currentStep === 'connect') && !streamProbing) {
			void probeCameraStreams(draft, streamProbeRevision);
		}
		if (stepIndex < cameraWizardSteps.length - 1) stepIndex += 1;
		error = null;
	}

	function back(): void {
		if (stepIndex > 0) stepIndex -= 1;
		error = null;
	}

	function mobileConnect(): void {
		let nextDraft = draft;
		if (!nextDraft.ip.trim() && manualAddress.trim()) {
			try {
				nextDraft = applyManualCameraAddress(nextDraft, manualAddress);
			} catch (cause) {
				error = cause instanceof Error ? cause.message : 'Camera address is invalid.';
				return;
			}
		}
		for (const step of ['find', 'connect'] as const) {
			const validationError = validateCameraWizardStep(step, nextDraft);
			if (validationError) {
				error = validationError;
				return;
			}
		}
		if (nextDraft !== draft) {
			draft = nextDraft;
			streamProbeRevision += 1;
		}
		if (!streamProbing) void probeCameraStreams(nextDraft, streamProbeRevision, true);
		stepIndex = cameraWizardSteps.indexOf('streams');
		error = null;
	}

	function mobileReview(): void {
		const validationError = validateCameraWizardStep('streams', draft) ?? streamVerificationError;
		if (validationError) {
			error = validationError;
			return;
		}
		stepIndex = cameraWizardSteps.indexOf('review');
		error = null;
	}

	async function save(): Promise<void> {
		if (saving) return;
		if (streamVerificationError) {
			error = streamVerificationError;
			return;
		}
		let update;
		try {
			update = cameraWizardUpdate(draft);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Camera configuration is invalid.';
			return;
		}
		saving = true;
		error = null;
		try {
			saved = await controlClient.updateCamera(draft.ip, update);
			draft = { ...draft, password: '' };
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Camera settings were not saved.';
		} finally {
			saving = false;
		}
	}

	function discard(): void {
		void goto(resolve('/cameras'));
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape' && saved === null) discard();
	}

	function evidenceLabel(camera: DiscoveredCameraSettings): string {
		return [camera.brand, camera.model, ...camera.sources]
			.filter((value): value is string => Boolean(value))
			.join(' · ');
	}

	function savedStateLabel(result: CameraSettingsUpdateResponse): string {
		if (result.restart_required) return 'RESTART REQUIRED';
		if (result.camera.health === 'online') return 'ONLINE';
		if (result.camera.health === 'degraded' || result.camera.health === 'stale') return 'DEGRADED';
		if (result.camera.health === 'offline') return 'OFFLINE';
		return 'STARTING';
	}

	function savedStateDetail(result: CameraSettingsUpdateResponse): string {
		if (result.restart_required) {
			return 'Saved to configuration. Restart KeepPeek before this camera can start.';
		}
		if (result.camera.health === 'online') {
			return 'Saved, started, and reporting online.';
		}
		if (result.camera.health === 'degraded' || result.camera.health === 'stale') {
			return 'Saved and started, but stream health needs attention.';
		}
		if (result.camera.health === 'offline') {
			return 'Saved, but the camera is currently offline.';
		}
		return 'Saved. KeepPeek is starting the camera and waiting for health evidence.';
	}
</script>

<svelte:head>
	<title>Add camera - KeepPeek</title>
</svelte:head>

<svelte:window onkeydown={handleKeydown} />

<div class="mx-auto max-w-5xl md:space-y-4 md:p-4">
	<header
		class="hidden min-h-11 flex-wrap items-center gap-3 border-b border-hairline pb-3 md:flex"
	>
		<button
			type="button"
			class="grid size-9 place-items-center rounded-sm border border-hairline-strong text-text-muted hover:bg-raised hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			aria-label="Cancel add camera"
			onclick={discard}
		>
			<ArrowLeftIcon class="size-4" />
		</button>
		<div>
			<h1 class="text-xl font-semibold">Add camera</h1>
			<p class="font-mono text-2xs tracking-caps text-text-faint">NOTHING SAVED UNTIL STEP 5</p>
		</div>
	</header>

	{#if saved}
		<section
			class="m-4 grid min-h-[28rem] place-items-center rounded-md border border-healthy/40 bg-surface p-6 text-center md:m-0"
			aria-label="Camera saved"
		>
			<div class="max-w-lg space-y-4">
				<span
					class="mx-auto grid size-12 place-items-center rounded-full bg-healthy/15 text-healthy"
					><CheckIcon class="size-6" /></span
				>
				<div>
					<h2 class="text-lg font-semibold">Camera saved</h2>
					<p
						class="mt-2 font-mono text-2xs tracking-caps {saved.camera.health === 'online'
							? 'text-healthy'
							: 'text-activity'}"
					>
						{savedStateLabel(saved)}
					</p>
					<p class="mt-1 text-sm text-text-muted">{savedStateDetail(saved)}</p>
				</div>
				<div class="flex flex-wrap justify-center gap-2">
					<a
						href={`${resolve('/camera')}?camera=${encodeURIComponent(saved.camera.id)}`}
						class="inline-flex h-9 items-center rounded-sm bg-primary px-4 text-xs font-semibold text-on-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						>Open camera</a
					>
					{#if saved.restart_required}<a
							href={`${resolve('/settings')}#appearance`}
							class="inline-flex h-9 items-center rounded-sm border border-hairline-strong bg-raised px-4 text-xs font-medium focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							>Restart KeepPeek</a
						>{:else if saved.camera.health !== 'online'}<a
							href={`${resolve('/system-health')}/camera/${encodeURIComponent(saved.camera.id)}`}
							class="inline-flex h-9 items-center rounded-sm border border-hairline-strong bg-raised px-4 text-xs font-medium focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
							>Open diagnostics</a
						>{/if}
					<a
						href={resolve('/cameras')}
						class="inline-flex h-9 items-center rounded-sm border border-hairline-strong bg-raised px-4 text-xs font-medium focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						>View fleet</a
					>
				</div>
			</div>
		</section>
	{:else}
		{#if mobileViewport}
			<MobileAddCameraWizard
				stage={mobileStage}
				{draft}
				{discovered}
				{selectedCatalogCamera}
				{catalogInfo}
				{catalogQuery}
				{catalogResults}
				{catalogSearching}
				{catalogSearchAttempted}
				{subnetPrefixes}
				{discoveryNetworks}
				{manualAddress}
				{manualAddressError}
				{manualAddressValid}
				{discovering}
				{discoveryElapsedMs}
				{discoveryAttempted}
				{discoveryCancelled}
				{error}
				{saving}
				{streamResolution}
				{streamProbeMessage}
				{streamProbing}
				streamEvidence={streamProbeResult?.streams ?? []}
				probe={streamProbeResult}
				{streamVerificationError}
				{catalogStreamsApplied}
				oncancel={discard}
				ondiscover={discover}
				oncanceldiscovery={cancelDiscovery}
				onselect={selectDiscovered}
				onapplycatalogstreams={applyCatalogStreams}
				oncatalogquery={updateCatalogQuery}
				onsearchcatalog={searchCatalog}
				onselectcatalog={selectCatalogCamera}
				onsubnets={setSubnetPrefixes}
				onmanualaddress={updateManualAddress}
				onupdate={updateDraft}
				onconnect={mobileConnect}
				onreview={mobileReview}
				onverifystreams={tryCameraConnection}
				onsave={save}
			/>
		{:else}
			<div class="hidden md:block" data-desktop-camera-wizard>
				<ol class="grid grid-cols-5 gap-1" aria-label="Add camera progress">
					{#each cameraWizardSteps as step, index (step)}
						<li class="min-w-0">
							<div
								class="h-1 rounded-full {index <= stepIndex ? 'bg-primary' : 'bg-hairline'}"
							></div>
							<p
								class="mt-1 truncate text-center font-mono text-2xs {index === stepIndex
									? 'text-primary-soft'
									: 'text-text-faint'}"
							>
								{index + 1} · {stepLabels[step]}
							</p>
						</li>
					{/each}
				</ol>

				<section
					class="overflow-hidden rounded-md border border-hairline bg-surface"
					aria-labelledby="wizard-step-heading"
				>
					<header class="flex min-h-14 items-center gap-3 border-b border-hairline px-4">
						<span
							class="grid size-7 shrink-0 place-items-center rounded-full bg-primary font-mono text-xs font-semibold text-on-primary"
							>{stepIndex + 1}</span
						>
						<h2 id="wizard-step-heading" class="text-base font-semibold">
							{stepLabels[currentStep]}
						</h2>
						<span class="ml-auto font-mono text-2xs tracking-caps text-text-faint"
							>STEP {stepIndex + 1} OF 5</span
						>
					</header>

					<div class="min-h-[28rem] p-4 md:p-6">
						{#if currentStep === 'find'}
							<div class="grid gap-5 lg:grid-cols-2">
								<div id="discover-camera" class="space-y-3">
									<h3 class="text-sm font-semibold">Discover on your network</h3>
									<p class="text-xs leading-5 text-text-muted">
										ONVIF, vendor discovery, and RTSP probes run in parallel. Most scans finish in
										about five seconds.
									</p>
									<label class="grid gap-1.5 text-xs font-medium"
										>Subnet prefixes
										<input
											class="h-9 rounded-sm border border-hairline bg-raised px-3 font-mono text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
											value={subnetPrefixes}
											oninput={(event) => setSubnetPrefixes(event.currentTarget.value)}
										/>
									</label>
									{#if discoveryNetworks.length > 0}
										<div class="flex flex-wrap gap-2" role="group" aria-label="Attached networks">
											{#each discoveryNetworks as network (network.cidr)}
												<button
													type="button"
													class="rounded-sm border px-2.5 py-1.5 font-mono text-2xs {subnetPrefixes
														.split(',')
														.map((value) => value.trim())
														.includes(subnetPrefix(network.cidr))
														? 'border-primary bg-primary/10 text-primary-soft'
														: 'border-hairline-strong text-text-muted'}"
													aria-pressed={subnetPrefixes
														.split(',')
														.map((value) => value.trim())
														.includes(subnetPrefix(network.cidr))}
													onclick={() => toggleDiscoveryNetwork(network)}
												>
													{network.cidr} · {network.interface_name}{network.preferred
														? ' · ACTIVE'
														: ''}
												</button>
											{/each}
										</div>
									{/if}
									<button
										type="button"
										class="inline-flex h-9 items-center gap-2 rounded-sm bg-primary px-4 text-xs font-semibold text-on-primary disabled:opacity-50"
										disabled={discovering}
										onclick={() => void discover()}
										><SearchIcon class="size-3.5" />{discovering
											? 'Scanning network'
											: 'Discover cameras'}</button
									>
									{#if discovering}<button
											type="button"
											class="ml-2 inline-flex h-9 items-center rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
											onclick={cancelDiscovery}>Cancel discovery</button
										>{/if}
									{#if discovering}
										<DiscoveryProgressState
											answeredCount={discovered.length}
											elapsedMs={discoveryElapsedMs}
											subnetCount={discoverySubnetCount}
											class="h-[172px] rounded-sm border border-hairline"
										/>
									{/if}
									{#if discoveryCancelled}
										<p class="text-xs text-text-muted" role="status">
											Discovery cancelled. Cameras already found remain available.
										</p>
									{/if}
									<div class="space-y-1.5" role="group" aria-label="Discovered cameras">
										{#each discovered as camera (camera.ip)}
											<button
												type="button"
												class="flex min-h-12 w-full items-center gap-3 rounded-sm border px-3 text-left disabled:cursor-not-allowed disabled:opacity-45 {draft.ip ===
												camera.ip
													? 'border-primary bg-primary/5'
													: 'border-hairline bg-raised'}"
												disabled={camera.configured}
												aria-pressed={draft.ip === camera.ip}
												onclick={() => selectDiscovered(camera)}
											>
												<span
													class="size-2 shrink-0 rounded-full {camera.configured
														? 'bg-text-faint'
														: 'bg-healthy'}"
												></span>
												<span class="min-w-0 flex-1"
													><span class="block truncate text-xs font-medium"
														>{camera.name ?? camera.ip}</span
													>
													<span class="block truncate font-mono text-2xs text-text-faint"
														>{camera.configured
															? 'ALREADY ADDED'
															: camera.catalog
																? `NETWORK · CATALOG ${camera.catalog.model}`
																: `NETWORK · ${evidenceLabel(camera)}`}</span
													></span
												>
											</button>
										{:else}
											{#if discoveryAttempted && !discovering}
												<p
													class="rounded-sm border border-dashed border-hairline-strong px-3 py-4 text-center text-xs text-text-muted"
												>
													No cameras answered. Manual entry still works.
												</p>
											{/if}
										{/each}
									</div>
									{#if selectedCatalogCamera && selectedCatalogFromDiscovery}
										<CameraCatalogEvidence camera={selectedCatalogCamera} {catalogInfo} />
									{/if}
								</div>
								<div
									id="manual-camera"
									class="space-y-3 border-t border-hairline pt-5 lg:border-t-0 lg:border-l lg:pt-0 lg:pl-5"
								>
									<h3 class="text-sm font-semibold">Connect directly</h3>
									<p class="text-xs leading-5 text-text-muted">
										Enter an address and sign-in to start ONVIF lookup immediately. A model is
										optional catalog context.
									</p>
									<label class="grid gap-1.5 text-xs font-medium"
										>Address or RTSP URL
										<input
											class="h-9 rounded-sm border border-hairline bg-raised px-3 font-mono text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
											value={manualAddress}
											placeholder="192.168.1.71 or rtsp://…"
											aria-invalid={manualAddressError ? true : undefined}
											aria-describedby={manualAddressError || manualAddressValid
												? 'desktop-manual-address-status'
												: undefined}
											oninput={(event) => updateManualAddress(event.currentTarget.value)}
										/>
									</label>
									{#if manualAddressError || manualAddressValid}<p
											id="desktop-manual-address-status"
											class="text-xs {manualAddressError ? 'text-live-text' : 'text-healthy'}"
											role="status"
										>
											{manualAddressError ?? 'Address format is ready to use.'}
										</p>{/if}
									<div class="grid gap-3 sm:grid-cols-2">
										<label class="grid gap-1.5 text-xs font-medium"
											>Username<input
												class="h-9 rounded-sm border border-hairline bg-raised px-3 text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
												value={draft.username}
												placeholder={draft.defaultUsernameConfigured
													? 'Configured default'
													: undefined}
												autocomplete="username"
												oninput={(event) => updateDraft({ username: event.currentTarget.value })}
											/></label
										>
										<label class="grid gap-1.5 text-xs font-medium"
											>Password<input
												type="password"
												class="h-9 rounded-sm border border-hairline bg-raised px-3 text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
												value={draft.password}
												placeholder={draft.defaultPasswordConfigured
													? 'Configured default'
													: undefined}
												autocomplete="new-password"
												oninput={(event) => updateDraft({ password: event.currentTarget.value })}
											/></label
										>
									</div>
									{#if (draft.defaultUsernameConfigured && !draft.username) || (draft.defaultPasswordConfigured && !draft.password)}
										<p class="text-xs text-healthy">
											Configured camera defaults are used without exposing their values.
										</p>
									{/if}
									<p
										data-onvif-probe-status
										class="flex items-start gap-2 rounded-sm border border-activity/40 bg-activity/10 px-3 py-2.5 text-xs leading-5 text-text-muted"
										role="status"
									>
										<LockIcon
											class="mt-0.5 size-3.5 shrink-0 text-activity"
										/>{firstScreenProbeStatus}
									</p>
									<label class="grid gap-1.5 text-xs font-medium"
										>Camera model (optional)
										<div class="flex gap-2">
											<input
												class="h-9 min-w-0 flex-1 rounded-sm border border-hairline bg-raised px-3 text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
												value={catalogQuery}
												placeholder="Search catalog"
												oninput={(event) => updateCatalogQuery(event.currentTarget.value)}
											/>
											<button
												type="button"
												class="h-9 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium disabled:opacity-50"
												disabled={!catalogQuery.trim() || catalogSearching}
												onclick={() => void searchCatalog()}
												>{catalogSearching ? 'Searching' : 'Search'}</button
											>
										</div>
									</label>
									{#if catalogResults.length > 0}
										<div class="space-y-1.5" role="group" aria-label="Camera catalog results">
											{#each catalogResults as camera (camera.id)}
												<button
													type="button"
													class="flex w-full items-center justify-between gap-3 rounded-sm border px-3 py-2 text-left text-xs {selectedCatalogCamera?.id ===
													camera.id
														? 'border-primary bg-primary/5'
														: 'border-hairline bg-raised'}"
													aria-pressed={selectedCatalogCamera?.id === camera.id}
													onclick={() => selectCatalogCamera(camera)}
												>
													<span class="min-w-0">
														<span class="block truncate font-medium"
															>{camera.brand} {camera.model}</span
														>
														<span class="block truncate font-mono text-2xs text-text-faint"
															>{[camera.camera_type, camera.resolution_label]
																.filter(Boolean)
																.join(' · ')}</span
														>
													</span>
													{#if selectedCatalogCamera?.id === camera.id}<span
															class="inline-flex shrink-0 items-center gap-1 text-primary-soft"
															><CheckIcon class="size-3.5" /> Selected</span
														>{/if}
												</button>
											{/each}
										</div>
									{:else if catalogSearchAttempted && !catalogSearching}
										<div
											class="rounded-sm border border-dashed border-hairline-strong px-3 py-3 text-xs leading-5 text-text-muted"
											role="status"
										>
											<p>
												No catalog results for {catalogQuery}. Try a brand and model, or continue
												with the manual address.
											</p>
											<a
												href={catalogInfo?.website_url ?? 'https://www.cctv-database.com/'}
												target="_blank"
												rel="noreferrer"
												class="mt-2 inline-flex items-center gap-1 font-medium text-primary-soft hover:text-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
											>
												Research on CCTV Database <ExternalLinkIcon class="size-3" />
											</a>
										</div>
									{/if}
									{#if selectedCatalogCamera && !selectedCatalogFromDiscovery}
										<CameraCatalogEvidence camera={selectedCatalogCamera} {catalogInfo} />
									{/if}
									{#if draft.discoveryEvidence}<p
											class="rounded-sm border border-healthy/30 bg-healthy/5 px-3 py-2.5 text-xs text-text-muted"
										>
											<span class="font-medium text-foreground">Selected {draft.ip}</span><br
											/>{draft.discoveryEvidence}
										</p>{/if}
								</div>
							</div>
						{:else if currentStep === 'connect'}
							<div class="mx-auto max-w-2xl space-y-4">
								<div>
									<p class="text-sm font-semibold">Tune a nonstandard camera</p>
									<p class="mt-1 text-xs leading-5 text-text-muted">
										ONVIF lookup began from the first screen. Adjust these options only when the
										camera needs a nonstandard connection.
									</p>
								</div>
								<div class="grid gap-4 sm:grid-cols-2">
									<label class="grid gap-1.5 text-xs font-medium"
										>Protocol<select
											class="h-9 rounded-sm border border-hairline bg-raised px-3 text-xs"
											value={draft.backend}
											onchange={(event) =>
												updateDraft({
													backend: event.currentTarget.value as CameraWizardDraft['backend']
												})}
											><option value="auto">Auto — recommended</option><option value="retina"
												>ONVIF / RTSP</option
											><option value="reo-proto">Reolink native</option></select
										></label
									>
									<label class="grid gap-1.5 text-xs font-medium"
										>Transport<select
											class="h-9 rounded-sm border border-hairline bg-raised px-3 text-xs"
											value={draft.transport}
											onchange={(event) =>
												updateDraft({
													transport: event.currentTarget.value as CameraWizardDraft['transport']
												})}><option value="tcp">TCP</option><option value="udp">UDP</option></select
										></label
									>
									<label class="grid gap-1.5 text-xs font-medium"
										>ONVIF port<input
											inputmode="numeric"
											class="h-9 rounded-sm border border-hairline bg-raised px-3 font-mono text-xs"
											value={draft.onvifPort}
											oninput={(event) => updateDraft({ onvifPort: event.currentTarget.value })}
										/></label
									>
									<label class="grid gap-1.5 text-xs font-medium"
										>HTTP port<input
											inputmode="numeric"
											class="h-9 rounded-sm border border-hairline bg-raised px-3 font-mono text-xs"
											value={draft.httpPort}
											oninput={(event) => updateDraft({ httpPort: event.currentTarget.value })}
										/></label
									>
								</div>
								<div
									class="flex items-center justify-between gap-3 rounded-md border border-activity bg-activity/10 px-3 py-2.5"
								>
									<p class="text-xs text-text-muted" role="status">
										{streamProbing
											? 'Connecting to ONVIF…'
											: streamResolution === 'onvif'
												? 'ONVIF stream endpoints are ready.'
												: (streamProbeMessage ?? 'Add a username and password to try the camera.')}
									</p>
									<button
										type="button"
										class="inline-flex h-9 shrink-0 items-center rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium disabled:opacity-50"
										disabled={!canProbeCameraStreams(draft) || streamProbing}
										onclick={tryCameraConnection}
										>{streamProbing ? 'Trying…' : 'Try ONVIF again'}</button
									>
								</div>
							</div>
						{:else if currentStep === 'streams'}
							<DesktopCameraWizardStreamsStep
								{draft}
								streamHints={selectedCatalogCamera?.stream_hints ?? null}
								{catalogStreamsApplied}
								{streamResolution}
								{streamProbeMessage}
								streamEvidence={streamProbeResult?.streams ?? []}
								verifying={streamProbing}
								onapplycatalogstreams={applyCatalogStreams}
								onverify={tryCameraConnection}
								onupdate={updateDraft}
							/>
						{:else if currentStep === 'recording'}
							<div class="mx-auto max-w-2xl space-y-4">
								<label class="grid gap-1.5 text-xs font-medium"
									>Camera name<input
										class="h-10 rounded-sm border border-hairline bg-raised px-3 text-sm outline-none focus:border-ring focus:ring-1 focus:ring-ring"
										value={draft.displayName}
										oninput={(event) => updateDraft({ displayName: event.currentTarget.value })}
									/></label
								>
								<div class="grid gap-3 sm:grid-cols-2">
									<label class="grid gap-1.5 text-xs font-medium"
										>Recording mode<select
											class="h-10 rounded-sm border border-hairline bg-raised px-3 text-xs"
											value={draft.recordingMode}
											onchange={(event) =>
												updateDraft({
													recordingMode: event.currentTarget
														.value as CameraWizardDraft['recordingMode']
												})}
											><option value="event-boost">Sub, main on events</option><option value="sub"
												>Sub only</option
											><option value="main">Main only</option><option value="both"
												>Main + sub</option
											><option value="off">Don't record</option></select
										></label
									>
									<div class="rounded-md border border-hairline bg-raised p-3">
										<p class="font-mono text-2xs tracking-caps text-text-faint">STREAM PROOF</p>
										<p class="mt-1 text-sm font-medium">
											{streamVerificationError ?? 'Required streams verified'}
										</p>
										<p class="mt-1 text-xs text-text-muted">
											Recording policy determines which keyframes must be proven.
										</p>
									</div>
								</div>
								{#if draft.recordingMode === 'event-boost'}<label
										class="grid gap-1.5 text-xs font-medium"
										>Main recording after an event (seconds)<input
											class="h-10 rounded-sm border border-hairline bg-raised px-3 font-mono text-xs"
											value={draft.eventRecordingDurationSeconds}
											oninput={(event) =>
												updateDraft({ eventRecordingDurationSeconds: event.currentTarget.value })}
										/></label
									>
									<p class="text-xs leading-5 text-text-muted">
										Substream GOPs are stored normally, main begins on an event keyframe, and
										recording returns to sub after this window.
									</p>{/if}
								<label
									class="flex items-start gap-3 rounded-sm border border-hairline bg-raised p-3"
								>
									<input
										type="checkbox"
										checked={draft.recordGenericMotionEvents}
										onchange={(event) =>
											updateDraft({ recordGenericMotionEvents: event.currentTarget.checked })}
										class="mt-0.5 size-4 accent-primary"
									/>
									<span class="text-xs leading-5"
										><strong>Store generic motion events</strong><br /><span class="text-text-muted"
											>Off keeps classified person, animal, and vehicle alarms.</span
										></span
									>
								</label>
							</div>
						{:else}
							<div class="mx-auto max-w-2xl space-y-4">
								<dl
									class="divide-y divide-hairline rounded-md border border-hairline bg-raised text-xs"
								>
									{#each [['Name', draft.displayName], ['Address', draft.ip], ['Protocol', draft.backend], ['Transport', draft.transport], ['Recording', draft.recordingMode], ['Main proof', streamProbeResult?.streams.find((stream) => stream.stream === 'main')?.verified ? 'Video + keyframe verified' : 'Not verified'], ['Sub proof', streamProbeResult?.streams.find((stream) => stream.stream === 'sub')?.verified ? 'Video + keyframe verified' : 'Not verified'], ['Credentials', credentialReview]] as item (item[0])}<div
											class="flex items-center justify-between gap-4 px-3 py-2.5"
										>
											<dt class="text-text-muted">{item[0]}</dt>
											<dd class="max-w-[70%] truncate text-right font-mono">{item[1]}</dd>
										</div>{/each}
								</dl>
								<CameraOnboardingEvidence
									catalogCamera={selectedCatalogCamera}
									{catalogInfo}
									probe={streamProbeResult}
								/>
								<p
									class="rounded-md border border-primary/30 bg-primary/5 px-3 py-2.5 text-xs leading-5 text-text-muted"
								>
									Saving is the first configuration write. The server may require a restart before
									recording begins.
								</p>
							</div>
						{/if}

						{#if error}<p
								class="mt-4 rounded-sm border border-destructive/40 bg-destructive/10 px-3 py-2.5 text-xs text-destructive"
								role="alert"
							>
								{error}
							</p>{/if}
					</div>

					<footer
						data-wizard-actions
						class="sticky bottom-[78px] z-30 flex min-h-14 items-center gap-2 border-t border-hairline bg-surface px-4 md:bottom-0"
					>
						<span class="mr-auto font-mono text-2xs tracking-caps text-text-faint"
							>ESC DISCARDS EVERYTHING</span
						>
						{#if stepIndex > 0}<button
								type="button"
								class="inline-flex h-9 items-center gap-1.5 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
								onclick={back}><ArrowLeftIcon class="size-3.5" />Back</button
							>{/if}
						{#if currentStep === 'review'}<button
								type="button"
								class="inline-flex h-9 items-center gap-1.5 rounded-sm bg-primary px-4 text-xs font-semibold text-on-primary disabled:opacity-50"
								disabled={saving || streamVerificationError !== null}
								onclick={() => void save()}
								><CheckIcon class="size-3.5" />{saving ? 'Saving…' : 'Save camera'}</button
							>{:else}<button
								type="button"
								class="inline-flex h-9 items-center gap-1.5 rounded-sm bg-primary px-4 text-xs font-semibold text-on-primary disabled:opacity-50"
								disabled={currentStep === 'streams' && streamVerificationError !== null}
								onclick={() => void next()}
								>{streamProbing ? 'Continue to streams' : 'Continue'}<ArrowRightIcon
									class="size-3.5"
								/></button
							>{/if}
					</footer>
				</section>
			</div>
		{/if}
	{/if}
</div>
