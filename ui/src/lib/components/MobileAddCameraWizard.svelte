<script lang="ts">
	import type { CameraWizardDraft } from '$lib/camera-wizard';
	import CameraCatalogEvidence from '$lib/components/CameraCatalogEvidence.svelte';
	import CameraOnboardingEvidence from '$lib/components/CameraOnboardingEvidence.svelte';
	import type {
		CameraCatalogCamera,
		CameraCatalogInfo,
		CameraDiscoveryNetwork,
		CameraStreamVerification,
		CameraStreamProbeResult,
		DiscoveredCameraSettings
	} from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import CircleCheckIcon from '@lucide/svelte/icons/circle-check';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import LockIcon from '@lucide/svelte/icons/lock';
	import LoaderCircleIcon from '@lucide/svelte/icons/loader-circle';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import SearchIcon from '@lucide/svelte/icons/search';
	import XIcon from '@lucide/svelte/icons/x';

	export type MobileCameraWizardStage = 'find-connect' | 'review' | 'streams';

	type Props = {
		stage: MobileCameraWizardStage;
		draft: CameraWizardDraft;
		discovered: readonly DiscoveredCameraSettings[];
		discoveryNetworks?: readonly CameraDiscoveryNetwork[];
		selectedCatalogCamera?: CameraCatalogCamera | null;
		catalogInfo?: CameraCatalogInfo | null;
		catalogQuery?: string;
		catalogResults?: readonly CameraCatalogCamera[];
		catalogSearching?: boolean;
		catalogSearchAttempted?: boolean;
		streamResolution?: 'unresolved' | 'catalog' | 'probing' | 'onvif' | 'manual';
		streamProbeMessage?: string | null;
		streamProbing?: boolean;
		streamEvidence?: readonly CameraStreamVerification[];
		probe?: CameraStreamProbeResult | null;
		streamVerificationError?: string | null;
		subnetPrefixes: string;
		manualAddress: string;
		manualAddressError?: string | null;
		manualAddressValid?: boolean;
		discovering: boolean;
		discoveryElapsedMs: number;
		discoveryAttempted: boolean;
		discoveryCancelled?: boolean;
		error: string | null;
		saving: boolean;
		catalogStreamsApplied?: boolean;
		actionFixed?: boolean;
		oncancel?: () => void;
		ondiscover?: () => void | Promise<void>;
		oncanceldiscovery?: () => void;
		onselect?: (camera: DiscoveredCameraSettings) => void;
		onapplycatalogstreams?: () => void;
		oncatalogquery?: (value: string) => void;
		onsearchcatalog?: () => void | Promise<void>;
		onselectcatalog?: (camera: CameraCatalogCamera) => void;
		onsubnets?: (value: string) => void;
		onmanualaddress?: (value: string) => void;
		onupdate?: (update: Partial<CameraWizardDraft>) => void;
		onconnect?: () => void | Promise<void>;
		onreview?: () => void;
		onverifystreams?: () => void;
		onsave?: () => void | Promise<void>;
	};

	let {
		stage,
		draft,
		discovered,
		discoveryNetworks = [],
		selectedCatalogCamera = null,
		catalogInfo = null,
		catalogQuery = '',
		catalogResults = [],
		catalogSearching = false,
		catalogSearchAttempted = false,
		streamResolution = 'unresolved',
		streamProbeMessage = null,
		streamProbing = false,
		streamEvidence = [],
		probe = null,
		streamVerificationError = null,
		subnetPrefixes,
		manualAddress,
		manualAddressError = null,
		manualAddressValid = false,
		discovering,
		discoveryElapsedMs,
		discoveryAttempted,
		discoveryCancelled = false,
		error,
		saving,
		catalogStreamsApplied = false,
		actionFixed = true,
		oncancel,
		ondiscover,
		oncanceldiscovery,
		onselect,
		onapplycatalogstreams,
		oncatalogquery,
		onsearchcatalog,
		onselectcatalog,
		onsubnets,
		onmanualaddress,
		onupdate,
		onconnect,
		onreview,
		onverifystreams,
		onsave
	}: Props = $props();

	let catalogPickerOpen = $state(false);
	let catalogStreamHints = $derived(selectedCatalogCamera?.stream_hints ?? null);
	let hasCatalogStreamHints = $derived(
		Boolean(catalogStreamHints?.main_rtsp_url || catalogStreamHints?.sub_rtsp_url)
	);
	let mainStreamEvidence = $derived(streamEvidenceFor('main'));
	let subStreamEvidence = $derived(streamEvidenceFor('sub'));
	let credentialSummary = $derived(
		draft.username || draft.password
			? 'Credentials provided'
			: draft.defaultUsernameConfigured && draft.defaultPasswordConfigured
				? 'Configured default'
				: 'Missing'
	);

	const stageDetails: Record<MobileCameraWizardStage, { label: string; number: number }> = {
		'find-connect': { label: 'Add a camera', number: 1 },
		streams: { label: 'Assign streams', number: 2 },
		review: { label: 'Review & save', number: 3 }
	};

	function evidenceLabel(camera: DiscoveredCameraSettings): string {
		return [camera.brand, camera.model, ...camera.sources]
			.filter((value): value is string => Boolean(value))
			.join(' · ');
	}

	function searchCatalog(event: SubmitEvent): void {
		event.preventDefault();
		void onsearchcatalog?.();
	}

	function selectCatalogCamera(camera: CameraCatalogCamera): void {
		onselectcatalog?.(camera);
		catalogPickerOpen = false;
	}

	function streamEvidenceFor(stream: 'main' | 'sub'): CameraStreamVerification | null {
		return streamEvidence.find((evidence) => evidence.stream === stream) ?? null;
	}

	function subnetPrefix(cidr: string): string {
		return cidr.endsWith('.0/24') ? cidr.slice(0, -5) : cidr;
	}
</script>

<section
	data-mobile-camera-wizard={stage}
	class="flex w-full flex-col md:hidden"
	aria-label={`Mobile add camera · ${stageDetails[stage].label}`}
>
	<header
		class="flex h-[52px] shrink-0 items-center justify-between gap-3 border-b border-hairline px-4"
	>
		<div class="flex min-w-0 items-center gap-3">
			<button
				type="button"
				class="grid size-[18px] shrink-0 place-items-center focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				aria-label="Cancel add camera"
				onclick={oncancel}
			>
				<ChevronLeftIcon class="size-[18px]" strokeWidth={2} />
			</button>
			<h1 class="truncate text-lg leading-5 font-semibold">{stageDetails[stage].label}</h1>
		</div>
		<span class="shrink-0 font-mono text-2xs leading-3 text-primary-soft">
			{stageDetails[stage].number} OF 3
		</span>
	</header>

	{#if stage === 'find-connect'}
		<div class="flex h-[660px] shrink-0 flex-col gap-[14px] overflow-y-auto p-4">
			<div class="h-[49px] shrink-0">
				<h2 class="text-xl leading-6 font-semibold">Find and connect</h2>
				<p class="mt-[5px] text-xs leading-5 text-text-muted">
					Parallel protocol probes usually finish in about five seconds. Nothing is saved yet.
				</p>
			</div>

			<div class="flex h-12 shrink-0 gap-2">
				<label class="min-w-0 flex-1">
					<span class="sr-only">Discovery network</span>
					<select
						class="h-12 w-full rounded-sm border border-hairline bg-raised px-3 font-mono text-2xs text-text-muted"
						value={subnetPrefixes.split(',')[0]?.trim()}
						onchange={(event) => onsubnets?.(event.currentTarget.value)}
					>
						{#each discoveryNetworks as network (network.cidr)}
							<option value={subnetPrefix(network.cidr)}>
								{network.cidr} · {network.interface_name}{network.preferred ? ' · active' : ''}
							</option>
						{:else}
							<option value={subnetPrefixes}>{subnetPrefixes}.0/24</option>
						{/each}
					</select>
				</label>
				<button
					type="button"
					class="h-12 shrink-0 rounded-sm bg-primary px-4 font-mono text-2xs font-semibold text-on-primary disabled:opacity-60"
					onclick={() => (discovering ? oncanceldiscovery?.() : void ondiscover?.())}
					aria-label={discovering ? 'Cancel camera discovery' : 'Scan this network'}
				>
					{discovering ? 'CANCEL' : 'SCAN'}
				</button>
			</div>
			{#if discoveryCancelled}<p class="text-xs text-text-muted" role="status">
					Discovery cancelled · found cameras remain available
				</p>{/if}

			<p class="h-3 shrink-0 font-mono text-2xs leading-3 text-text-faint uppercase">
				{discovered.length} found
			</p>
			<div
				class="flex h-[154px] shrink-0 flex-col gap-[10px]"
				role="group"
				aria-label="Discovered cameras"
			>
				{#each discovered.slice(0, 2) as camera (camera.ip)}
					<button
						type="button"
						class="flex h-[70px] shrink-0 items-center gap-[11px] rounded-sm border px-[14px] text-left disabled:opacity-45 {draft.ip ===
						camera.ip
							? 'border-primary bg-primary/5'
							: 'border-hairline bg-surface'}"
						disabled={camera.configured}
						aria-pressed={draft.ip === camera.ip}
						onclick={() => onselect?.(camera)}
					>
						<span
							class="size-[7px] shrink-0 rounded-full {camera.configured
								? 'bg-text-faint'
								: 'bg-healthy'}"
						></span>
						<span class="min-w-0 flex-1">
							<span class="block truncate text-sm leading-4 font-semibold"
								>{camera.name ?? camera.ip}</span
							>
							<span class="mt-1 block truncate font-mono text-2xs leading-3 text-text-faint">
								{#if camera.configured}
									ALREADY ADDED
								{:else}
									<span class="text-healthy">NETWORK</span>
									{#if camera.catalog}
										<span> · </span><span class="text-primary-soft"
											>CATALOG · {camera.catalog.model}</span
										>
									{:else}
										<span> · {evidenceLabel(camera)}</span>
									{/if}
								{/if}
							</span>
						</span>
						{#if draft.ip === camera.ip}<span class="text-xs leading-4 text-primary-soft"
								>Selected</span
							>{/if}
					</button>
				{/each}
				{#if discovered.length === 0}
					<div
						class="grid h-[58px] place-items-center rounded-sm border border-dashed border-hairline-strong text-xs text-text-muted"
					>
						{discoveryAttempted ? 'No cameras answered' : 'Scan to find cameras'}
					</div>
				{/if}
			</div>
			{#if selectedCatalogCamera}
				<CameraCatalogEvidence camera={selectedCatalogCamera} {catalogInfo} compact />
			{/if}

			<div
				class="flex h-[104px] shrink-0 flex-col gap-2.5 rounded-md border border-hairline bg-surface p-[14px]"
			>
				<div class="flex h-[18px] items-center justify-between">
					<h2 class="text-md leading-[18px] font-semibold">Sign-in</h2>
					<span class="font-mono text-2xs leading-3 text-text-faint">WRITE-ONLY DRAFT</span>
				</div>
				<div
					class="grid h-9 grid-cols-2 gap-2 rounded-sm border border-hairline-strong bg-raised px-2.5"
				>
					<label class="flex min-w-0 items-center gap-1.5">
						<span class="sr-only">Username</span><LockIcon
							class="size-3 shrink-0 text-text-faint"
						/>
						<input
							class="min-w-0 flex-1 bg-transparent font-mono text-xs outline-none"
							value={draft.username}
							placeholder={draft.defaultUsernameConfigured ? 'Configured default' : 'Username'}
							autocomplete="username"
							oninput={(event) => onupdate?.({ username: event.currentTarget.value })}
						/>
					</label>
					<label class="flex min-w-0 items-center border-l border-hairline pl-2">
						<span class="sr-only">Password</span>
						<input
							type="password"
							class="min-w-0 flex-1 bg-transparent font-mono text-xs outline-none"
							value={draft.password}
							placeholder={draft.defaultPasswordConfigured ? 'Configured default' : 'Password'}
							autocomplete="new-password"
							oninput={(event) => onupdate?.({ password: event.currentTarget.value })}
						/>
					</label>
				</div>
				<p class="text-xs leading-4 text-primary-soft">
					{streamProbing
						? 'Trying ONVIF…'
						: streamResolution === 'onvif'
							? `ONVIF stream endpoints are ready${draft.onvifPort ? ` on port ${draft.onvifPort}` : ''}`
							: (streamProbeMessage ?? 'Valid address and credentials start ONVIF automatically')}
				</p>
			</div>
		</div>
	{:else if stage === 'streams'}
		<div class="flex h-[660px] shrink-0 flex-col gap-[14px] overflow-hidden p-4">
			{#if hasCatalogStreamHints}
				<button
					type="button"
					class="inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-sm border border-primary/60 bg-primary/5 px-3 text-xs font-medium text-primary-soft"
					aria-pressed={catalogStreamsApplied}
					onclick={onapplycatalogstreams}
				>
					{#if catalogStreamsApplied}<CheckIcon class="size-3.5" />{/if}
					{catalogStreamsApplied ? 'Catalog streams applied' : 'Restore catalog streams'}</button
				>
			{/if}
			<p class="h-10 shrink-0 text-xs leading-5 text-text-muted">
				{streamResolution === 'probing'
					? 'ONVIF lookup is in progress. You can enter RTSP URLs manually now.'
					: streamResolution === 'onvif'
						? 'ONVIF reported candidate RTSP endpoints. You can edit either URL before saving.'
						: streamResolution === 'catalog'
							? 'Catalog candidate RTSP endpoints are applied. You can edit either URL before saving.'
							: (streamProbeMessage ??
								'No candidate endpoint was found. Enter RTSP URLs manually if needed.')}
			</p>
			<div class="h-[250px] shrink-0 overflow-hidden rounded-md border border-primary bg-surface">
				<div class="grid h-[170px] place-items-center bg-video">
					<div class="text-center">
						{#if mainStreamEvidence?.verified}<CheckIcon
								class="mx-auto size-5 text-healthy"
							/>{:else}<RadioIcon class="mx-auto size-5 text-text-faint" />{/if}
						<p class="mt-2 font-mono text-2xs leading-3 text-text-faint">
							{mainStreamEvidence?.verified
								? `${mainStreamEvidence.codec?.toUpperCase()} · ${mainStreamEvidence.resolution} · KEYFRAME`
								: (mainStreamEvidence?.error ?? 'NOT VERIFIED')}
						</p>
					</div>
				</div>
				<label class="block h-[78px] p-3 text-sm leading-4 font-semibold">
					Recording stream
					<input
						class="mt-2 h-8 w-full rounded-sm border border-hairline-strong bg-raised px-2.5 font-mono text-2xs font-normal outline-none"
						value={draft.mainRtspUrl}
						placeholder="Automatic main stream"
						oninput={(event) => onupdate?.({ mainRtspUrl: event.currentTarget.value })}
					/>
				</label>
			</div>
			<label
				class="grid h-[90px] shrink-0 grid-cols-[94px_minmax(0,1fr)] gap-3 rounded-md border border-hairline bg-surface p-[14px]"
			>
				<span class="grid h-[62px] place-items-center bg-video"
					>{#if subStreamEvidence?.verified}<CheckIcon
							class="size-4 text-healthy"
						/>{:else}<RadioIcon class="size-4 text-text-faint" />{/if}</span
				>
				<span class="min-w-0 text-sm leading-4 font-semibold">
					Live stream
					<input
						class="mt-2 h-8 w-full rounded-sm border border-hairline-strong bg-raised px-2.5 font-mono text-2xs font-normal outline-none"
						value={draft.subRtspUrl}
						placeholder="Automatic sub stream"
						oninput={(event) => onupdate?.({ subRtspUrl: event.currentTarget.value })}
					/>
				</span>
			</label>
			<label
				class="flex min-h-[58px] shrink-0 flex-col gap-[5px] font-mono text-2xs leading-3 text-text-faint"
			>
				RECORDING MODE
				<select
					class="h-10 rounded-sm border border-hairline-strong bg-raised px-3 font-sans text-xs text-text"
					value={draft.recordingMode}
					onchange={(event) =>
						onupdate?.({
							recordingMode: event.currentTarget.value as CameraWizardDraft['recordingMode']
						})}
				>
					<option value="event-boost">Sub, switch to main on events</option>
					<option value="sub">Sub only</option>
					<option value="main">Main only</option>
					<option value="both">Main + sub</option>
					<option value="off">Don't record</option>
				</select>
			</label>
			{#if draft.recordingMode === 'event-boost'}
				<label
					class="flex min-h-[58px] shrink-0 flex-col gap-[5px] font-mono text-2xs leading-3 text-text-faint"
				>
					MAIN AFTER EVENT · SECONDS
					<input
						class="h-10 rounded-sm border border-hairline-strong bg-raised px-3 text-xs text-text"
						value={draft.eventRecordingDurationSeconds}
						inputmode="numeric"
						oninput={(event) =>
							onupdate?.({ eventRecordingDurationSeconds: event.currentTarget.value })}
					/>
				</label>
			{/if}
			<div
				class="flex h-[62px] shrink-0 gap-3 rounded-sm border border-activity/40 bg-activity/10 p-3"
			>
				<span class="font-mono text-2xs leading-6 text-activity">PROOF</span>
				<p class="text-xs-plus leading-[18px] text-text-muted">
					{streamProbing
						? 'Authenticating and waiting for main and sub keyframes.'
						: (streamVerificationError ?? 'Required video streams and keyframes are verified.')}
				</p>
			</div>
		</div>
	{:else}
		<div class="flex h-[660px] shrink-0 flex-col gap-[14px] overflow-hidden p-4">
			<label
				class="flex h-[76px] shrink-0 flex-col gap-[5px] font-mono text-2xs leading-3 text-text-faint"
			>
				CAMERA NAME
				<input
					class="h-10 rounded-sm border border-primary bg-raised px-3 font-sans text-md leading-[18px] text-text outline-none"
					value={draft.displayName}
					oninput={(event) => onupdate?.({ displayName: event.currentTarget.value })}
				/>
				<span>SOURCE ID ASSIGNED BY SERVER · PERMANENT</span>
			</label>
			<dl class="h-[220px] shrink-0 rounded-md border border-hairline bg-surface p-[14px]">
				{#each [['Address', draft.ip], ['Sign-in', credentialSummary], ['Recording', draft.recordingMode], ['Main proof', streamEvidenceFor('main')?.verified ? 'Video + keyframe' : 'Not verified'], ['Sub proof', streamEvidenceFor('sub')?.verified ? 'Video + keyframe' : 'Not verified']] as item (item[0])}
					<div
						class="flex h-[38px] items-center justify-between border-b border-hairline last:border-b-0"
					>
						<dt class="text-sm leading-4 text-text-muted">{item[0]}</dt>
						<dd class="max-w-[68%] truncate text-right font-mono text-xs-plus leading-4">
							{item[1]}
						</dd>
					</div>
				{/each}
			</dl>
			<div
				class="flex h-[72px] shrink-0 flex-col gap-2 rounded-sm border-l-2 border-activity bg-raised p-3"
			>
				<div class="flex items-center justify-between">
					<p class="text-md leading-[18px] font-semibold">Retention impact unavailable</p>
					<span class="font-mono text-2xs leading-3 text-activity">NO PROJECTION API</span>
				</div>
				<p class="text-xs leading-[18px] text-text-muted">
					{draft.recordingMode === 'event-boost'
						? `Sub normally, main on events for ${draft.eventRecordingDurationSeconds}s.`
						: `Recording mode: ${draft.recordingMode}.`}
				</p>
			</div>
			<div
				class="flex h-[42px] shrink-0 items-center gap-2 rounded-sm border border-activity/35 bg-activity/10 px-3 text-xs leading-4 text-text-muted"
			>
				<span class="size-1.5 shrink-0 rounded-full bg-activity"></span>
				{streamVerificationError ?? 'Authenticated video and required keyframes are verified.'}
			</div>
			<p class="h-[18px] shrink-0 text-xs-plus leading-[18px] text-text-faint">
				Saving is the first configuration write.
			</p>
			<CameraOnboardingEvidence
				catalogCamera={selectedCatalogCamera}
				{catalogInfo}
				{probe}
				compact
			/>
		</div>
	{/if}

	<footer
		data-mobile-wizard-actions
		class="{actionFixed
			? 'fixed inset-x-0 bottom-0 z-50'
			: 'relative'} flex shrink-0 flex-col border-t border-hairline bg-surface px-4 {error
			? 'h-[100px] gap-1 pt-2 pb-3'
			: 'h-[68px] pt-2.5 pb-5'} md:hidden"
	>
		{#if error}<p class="w-full text-xs leading-4 text-live-text" role="alert">{error}</p>{/if}
		<div class="flex w-full items-start gap-2">
			{#if stage === 'find-connect'}
				<button
					type="button"
					class="inline-flex h-9 shrink-0 items-center gap-1 rounded-sm border border-hairline-strong bg-raised px-2 text-xs font-medium text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					aria-label="Browse camera models"
					title="Browse camera models"
					onclick={() => (catalogPickerOpen = true)}
				>
					<SearchIcon class="size-3.5" /> Model
				</button>
				<div class="relative h-9 min-w-0 flex-1">
					<input
						class="h-9 w-full bg-transparent pr-6 text-xs-plus leading-4 outline-none placeholder:text-text-faint"
						value={manualAddress}
						placeholder="IP or rtsp://"
						aria-label="Address or RTSP URL"
						aria-invalid={manualAddressError ? true : undefined}
						aria-describedby={manualAddressError || manualAddressValid
							? 'mobile-manual-address-status'
							: undefined}
						oninput={(event) => onmanualaddress?.(event.currentTarget.value)}
					/>
					{#if manualAddressValid}
						<CircleCheckIcon
							class="pointer-events-none absolute top-2 right-0 size-4 text-healthy"
							aria-hidden="true"
						/>
					{:else if manualAddressError}
						<CircleAlertIcon
							class="pointer-events-none absolute top-2 right-0 size-4 text-live-text"
							aria-hidden="true"
						/>
					{/if}
					{#if manualAddressError || manualAddressValid}<span
							id="mobile-manual-address-status"
							class="sr-only">{manualAddressError ?? 'Address format is ready to use.'}</span
						>{/if}
				</div>
				<button
					type="button"
					class="h-9 rounded-sm bg-primary px-4 text-md leading-[18px] font-semibold text-on-primary disabled:opacity-50"
					onclick={() => void onconnect?.()}>{streamProbing ? 'Continue' : 'Connect'}</button
				>
			{:else if stage === 'streams'}
				<button
					type="button"
					class="inline-flex h-9 items-center gap-1.5 text-xs-plus leading-4 text-primary-soft disabled:opacity-50"
					disabled={streamProbing}
					onclick={onverifystreams}
				>
					{streamProbing ? 'Verifying streams' : 'Verify streams'}
				</button>
				<button
					type="button"
					class="ml-auto h-9 rounded-sm bg-primary px-4 text-md leading-[18px] font-semibold text-on-primary disabled:opacity-45"
					disabled={streamVerificationError !== null}
					onclick={onreview}>Review</button
				>
			{:else}
				<button
					type="button"
					class="h-[38px] w-full rounded-sm bg-primary text-md leading-[18px] font-semibold text-on-primary disabled:opacity-45"
					disabled={saving || streamVerificationError !== null}
					onclick={() => void onsave?.()}
				>
					{saving ? 'Saving camera' : 'Save camera'}
				</button>
			{/if}
		</div>
	</footer>

	{#if catalogPickerOpen}
		<div class="fixed inset-0 z-[70] grid place-items-end bg-foreground/35 p-4 md:hidden">
			<dialog
				open
				class="m-0 flex max-h-[calc(100dvh-32px)] w-full max-w-md flex-col overflow-hidden rounded-md border border-hairline-strong bg-surface text-foreground shadow-xl"
				aria-labelledby="mobile-catalog-picker-title"
			>
				<header class="flex items-center justify-between gap-3 border-b border-hairline px-4 py-3">
					<h2 id="mobile-catalog-picker-title" class="text-base font-semibold">Camera model</h2>
					<button
						type="button"
						class="grid size-8 shrink-0 place-items-center rounded-sm text-text-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						aria-label="Close camera catalog"
						title="Close"
						onclick={() => (catalogPickerOpen = false)}
					>
						<XIcon class="size-4" />
					</button>
				</header>
				<form class="flex gap-2 border-b border-hairline p-4" onsubmit={searchCatalog}>
					<label class="sr-only" for="mobile-catalog-query">Camera model</label>
					<input
						id="mobile-catalog-query"
						class="h-9 min-w-0 flex-1 rounded-sm border border-hairline bg-raised px-3 text-xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
						value={catalogQuery}
						placeholder="Search catalog"
						autocomplete="off"
						oninput={(event) => oncatalogquery?.(event.currentTarget.value)}
					/>
					<button
						type="submit"
						class="h-9 shrink-0 rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary disabled:opacity-50"
						disabled={!catalogQuery.trim() || catalogSearching}
					>
						{catalogSearching ? 'Searching' : 'Search'}
					</button>
				</form>
				<div class="min-h-0 overflow-y-auto p-4" aria-live="polite">
					{#if catalogSearching}
						<div
							class="flex min-h-28 flex-col items-center justify-center gap-2 text-center"
							role="status"
						>
							<LoaderCircleIcon class="size-5 animate-spin text-primary-soft" />
							<p class="text-xs text-text-muted">Searching camera models</p>
						</div>
					{:else if catalogSearchAttempted && catalogResults.length === 0}
						<div
							class="grid min-h-28 place-items-center rounded-sm border border-dashed border-hairline-strong px-5 text-center"
							role="status"
						>
							<div>
								<p class="text-sm font-medium">No catalog results for {catalogQuery}</p>
								<p class="mt-1 text-xs leading-5 text-text-muted">
									Try a brand and model, or continue with the manual address.
								</p>
								<a
									href={catalogInfo?.website_url ?? 'https://www.cctv-database.com/'}
									target="_blank"
									rel="noreferrer"
									class="mt-3 inline-flex items-center gap-1 text-xs font-medium text-primary-soft hover:text-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								>
									Research on CCTV Database <ExternalLinkIcon class="size-3" />
								</a>
							</div>
						</div>
					{:else if catalogResults.length > 0}
						<div role="group" aria-label="Camera catalog results">
							{#each catalogResults as camera (camera.id)}
								<button
									type="button"
									class="flex w-full items-center justify-between gap-3 border-b border-hairline px-1 py-3 text-left last:border-b-0"
									aria-pressed={selectedCatalogCamera?.id === camera.id}
									onclick={() => selectCatalogCamera(camera)}
								>
									<span class="min-w-0">
										<span class="block truncate text-sm font-medium"
											>{camera.brand} {camera.model}</span
										>
										<span class="mt-1 block truncate font-mono text-2xs text-text-faint"
											>{[camera.camera_type, camera.resolution_label]
												.filter(Boolean)
												.join(' · ')}</span
										>
									</span>
									{#if selectedCatalogCamera?.id === camera.id}<CheckIcon
											class="size-4 shrink-0 text-primary-soft"
										/>{/if}
								</button>
							{/each}
						</div>
					{:else}
						<div class="grid min-h-28 place-items-center px-5 text-center">
							<p class="text-xs leading-5 text-text-muted">
								Search by brand or model to add a reference to this draft.
							</p>
						</div>
					{/if}
				</div>
			</dialog>
		</div>
	{/if}
</section>
