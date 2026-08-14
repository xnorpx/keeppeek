<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import {
		getCameraDetails,
		getServerHealth,
		setCameraManufacturer,
		setCameraMotionDetection
	} from '$lib/api';
	import type {
		CameraDetailsResponse,
		CameraHealth,
		MotionDetection,
		StreamHealth
	} from '$lib/types';
	import ArrowLeftIcon from '@lucide/svelte/icons/arrow-left';
	import CheckIcon from '@lucide/svelte/icons/check';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import XIcon from '@lucide/svelte/icons/x';

	const REFRESH_INTERVAL_MS = 5_000;
	const numberFormatter = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
	const compactFormatter = new Intl.NumberFormat(undefined, {
		notation: 'compact',
		maximumFractionDigits: 1
	});

	let details = $state.raw<CameraDetailsResponse | null>(null);
	let serverHealth = $state.raw<CameraHealth | null>(null);
	let error = $state<string | null>(null);
	let motionError = $state<string | null>(null);
	let loading = $state(true);
	let refreshing = $state(false);
	let updatingMotion = $state(false);
	let editingManufacturer = $state(false);
	let manufacturerDraft = $state('');
	let manufacturerError = $state<string | null>(null);
	let updatingManufacturer = $state(false);
	let cameraId = $derived(page.url.searchParams.get('camera')?.trim() ?? '');
	let camera = $derived(details?.camera ?? null);
	let liveHealth = $derived(serverHealth ?? details?.health ?? null);
	let capabilityItems = $derived.by(() => {
		const capabilities = camera?.capabilities;
		if (!capabilities) return [];
		return [
			['PTZ', capabilities.ptz],
			['Audio', capabilities.audio],
			['Events', capabilities.events],
			['Recording', capabilities.recording],
			['Analytics', capabilities.analytics],
			['Imaging', capabilities.imaging],
			['Two-way audio', capabilities.two_way_audio]
		];
	});

	$effect(() => {
		const id = cameraId;
		if (!id) {
			details = null;
			serverHealth = null;
			error = 'Choose a camera from Peek or Health.';
			loading = false;
			return;
		}

		const controller = new AbortController();
		loading = true;
		error = null;
		motionError = null;
		manufacturerError = null;
		editingManufacturer = false;
		void loadCamera(id, controller.signal);
		const timer = window.setInterval(() => {
			if (document.visibilityState === 'visible') void refreshHealth(id);
		}, REFRESH_INTERVAL_MS);
		return () => {
			controller.abort();
			window.clearInterval(timer);
		};
	});

	async function loadCamera(id: string, signal?: AbortSignal) {
		try {
			const [nextDetails, nextHealth] = await Promise.all([
				getCameraDetails(id, signal),
				getServerHealth(signal)
			]);
			if (signal?.aborted || id !== cameraId) return;
			details = nextDetails;
			serverHealth = nextHealth.cameras.find((candidate) => candidate.id === id) ?? null;
			error = null;
		} catch (cause) {
			if (signal?.aborted) return;
			error = cause instanceof Error ? cause.message : 'Camera information is unavailable.';
		} finally {
			if (!signal?.aborted && id === cameraId) loading = false;
		}
	}

	async function refreshHealth(id: string) {
		try {
			const nextHealth = await getServerHealth();
			if (id !== cameraId) return;
			serverHealth = nextHealth.cameras.find((candidate) => candidate.id === id) ?? null;
		} catch {
			// Keep the last successful health sample visible during a transient refresh failure.
		}
	}

	async function refreshCamera() {
		if (!cameraId) return;
		refreshing = true;
		await loadCamera(cameraId);
		refreshing = false;
	}

	async function updateMotion(enabled: boolean) {
		if (!camera || updatingMotion) return;
		updatingMotion = true;
		motionError = null;
		try {
			const motionDetection = await setCameraMotionDetection(camera.id, enabled);
			if (details?.camera.id === camera.id) {
				details = { ...details, motion_detection: motionDetection };
			}
		} catch (cause) {
			motionError =
				cause instanceof Error ? cause.message : 'Camera motion setting was not updated.';
		} finally {
			updatingMotion = false;
		}
	}

	function handleMotionChange(event: Event) {
		void updateMotion((event.currentTarget as HTMLInputElement).checked);
	}

	function editManufacturer() {
		if (!camera) return;
		manufacturerDraft = camera.manufacturer ?? '';
		manufacturerError = null;
		editingManufacturer = true;
	}

	function cancelManufacturerEdit() {
		editingManufacturer = false;
		manufacturerError = null;
	}

	function applyManufacturer(camera: NonNullable<CameraDetailsResponse['camera']>) {
		if (details?.camera.id === camera.id) {
			details = { ...details, camera };
		}
	}

	async function saveManufacturer(manufacturer: string | null) {
		if (!camera || updatingManufacturer) return;
		updatingManufacturer = true;
		manufacturerError = null;
		try {
			applyManufacturer(await setCameraManufacturer(camera.id, manufacturer));
			editingManufacturer = false;
		} catch (cause) {
			manufacturerError =
				cause instanceof Error ? cause.message : 'Manufacturer override was not saved.';
		} finally {
			updatingManufacturer = false;
		}
	}

	function handleManufacturerKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter') {
			event.preventDefault();
			void saveManufacturer(manufacturerDraft);
		} else if (event.key === 'Escape') {
			cancelManufacturerEdit();
		}
	}

	function formatBitrate(kbps: number | null | undefined): string {
		if (kbps === null || kbps === undefined) return '—';
		return `${numberFormatter.format(kbps)} kbps`;
	}

	function formatBytes(bytes: number | null | undefined): string {
		if (bytes === null || bytes === undefined) return '—';
		if (bytes < 1_000) return `${bytes} B`;
		const units = ['kB', 'MB', 'GB', 'TB'];
		let value = bytes / 1_000;
		let unit = 0;
		while (value >= 1_000 && unit < units.length - 1) {
			value /= 1_000;
			unit += 1;
		}
		return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
	}

	function formatFrameSize(kilobytes: number | null | undefined): string {
		return kilobytes && kilobytes > 0 ? formatBytes(kilobytes * 1_000) : '—';
	}

	function formatAge(milliseconds: number | null | undefined): string {
		if (milliseconds === null || milliseconds === undefined) return '—';
		if (milliseconds < 1_000) return 'now';
		if (milliseconds < 60_000) return `${Math.round(milliseconds / 1_000)}s`;
		return `${Math.round(milliseconds / 60_000)}m`;
	}

	function streamLabel(stream: StreamHealth): string {
		if (stream.type === 'video_main') return 'Main video';
		if (stream.type === 'video_sub') return 'Sub video';
		if (stream.type === 'audio') return 'Audio';
		return stream.type.replaceAll('_', ' ');
	}

	function isAudioStream(stream: StreamHealth): boolean {
		return stream.type === 'audio';
	}

	function stateClass(state: CameraHealth['state'] | undefined): string {
		if (state === 'online') return 'bg-emerald-500';
		if (state === 'starting') return 'bg-sky-500';
		if (state === 'degraded' || state === 'stale') return 'bg-amber-500';
		return 'bg-red-500';
	}

	function portSummary(): string {
		if (!camera?.ports) return 'Unavailable';
		const ports = [
			camera.ports.http && `HTTP ${camera.ports.http}`,
			camera.ports.https && `HTTPS ${camera.ports.https}`,
			camera.ports.rtsp && `RTSP ${camera.ports.rtsp}`,
			camera.ports.onvif && `ONVIF ${camera.ports.onvif}`
		].filter(Boolean);
		return ports.length > 0 ? ports.join(' · ') : 'Unavailable';
	}

	function motionSummary(motionDetection: MotionDetection): string {
		if (motionDetection.enabled === true) return 'Enabled';
		if (motionDetection.enabled === false) return 'Disabled';
		return motionDetection.supported ? 'State unavailable' : 'Not reported';
	}
</script>

<svelte:head>
	<title>{camera ? `${camera.name ?? camera.id} - KeepPeek` : 'Camera - KeepPeek'}</title>
</svelte:head>

<div class="mx-auto max-w-[120rem] space-y-6">
	<header class="flex flex-wrap items-center gap-3 border-b pb-4">
		<a
			href={resolve('/')}
			class="grid size-9 place-items-center rounded-md border text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			title="Return to Peek"
		>
			<ArrowLeftIcon class="size-4" />
			<span class="sr-only">Return to Peek</span>
		</a>
		<div class="min-w-0 flex-1">
			<p class="text-[10px] font-semibold text-muted-foreground uppercase">Camera</p>
			<h1 class="truncate text-xl font-semibold">
				{camera?.name ?? camera?.id ?? 'Camera information'}
			</h1>
			{#if camera}
				<p class="mt-0.5 font-mono text-xs text-muted-foreground">{camera.ip}</p>
			{/if}
		</div>
		{#if camera}
			<button
				type="button"
				class="grid size-9 place-items-center rounded-md border text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50"
				title="Refresh camera information"
				aria-label="Refresh camera information"
				disabled={refreshing}
				onclick={() => void refreshCamera()}
			>
				<RefreshCwIcon class="size-4 {refreshing ? 'animate-spin' : ''}" />
			</button>
		{/if}
	</header>

	{#if camera}
		<div class="-mt-4 flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
			<!-- eslint-disable svelte/no-navigation-without-resolve -->
			<a
				href={camera.web_url ?? `http://${camera.ip}`}
				target="_blank"
				rel="noopener noreferrer"
				class="inline-flex h-8 items-center gap-2 rounded-md border px-3 text-xs font-medium text-foreground hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			>
				<ExternalLinkIcon class="size-3.5" />
				Open camera UI
			</a>
			<!-- eslint-enable svelte/no-navigation-without-resolve -->
			<p>The camera UI link works only from a device on the same network as the camera.</p>
		</div>
	{/if}

	{#if loading && !details}
		<div class="grid min-h-56 place-items-center border-y text-sm text-muted-foreground">
			Loading camera information
		</div>
	{:else if error && !details}
		<div
			class="border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
			role="alert"
		>
			{error}
		</div>
	{:else if details && camera}
		<section
			class="grid gap-x-8 gap-y-5 border-y py-5 lg:grid-cols-[minmax(0,1fr)_minmax(18rem,0.7fr)]"
			aria-labelledby="camera-identity-heading"
		>
			<div>
				<div class="mb-3 flex items-center gap-2">
					<span class="size-2 rounded-full {stateClass(liveHealth?.state)}"></span>
					<h2 id="camera-identity-heading" class="text-sm font-semibold capitalize">
						{liveHealth?.state ?? 'Unknown'}
					</h2>
					{#if liveHealth?.lifecycle}
						<span class="text-xs text-muted-foreground">{liveHealth.lifecycle}</span>
					{/if}
				</div>
				<dl class="grid grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)] gap-x-5 gap-y-2 text-xs">
					<dt class="text-muted-foreground">Manufacturer</dt>
					<dd class="flex min-w-0 justify-end gap-1">
						{#if editingManufacturer}
							<input
								class="h-7 min-w-0 flex-1 rounded-md border bg-background px-2 text-right text-xs focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								aria-label="Manufacturer"
								bind:value={manufacturerDraft}
								onkeydown={handleManufacturerKeydown}
							/>
							<button
								type="button"
								class="grid size-7 shrink-0 place-items-center rounded-md border hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50"
								title="Save manufacturer"
								aria-label="Save manufacturer"
								disabled={updatingManufacturer}
								onclick={() => void saveManufacturer(manufacturerDraft)}
							>
								<CheckIcon class="size-3.5" />
							</button>
							<button
								type="button"
								class="grid size-7 shrink-0 place-items-center rounded-md border hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50"
								title="Cancel manufacturer edit"
								aria-label="Cancel manufacturer edit"
								disabled={updatingManufacturer}
								onclick={cancelManufacturerEdit}
							>
								<XIcon class="size-3.5" />
							</button>
						{:else}
							<span class="min-w-0 truncate">{camera.manufacturer ?? 'Unknown'}</span>
							<button
								type="button"
								class="grid size-7 shrink-0 place-items-center rounded-md border hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								title="Edit manufacturer"
								aria-label="Edit manufacturer"
								onclick={editManufacturer}
							>
								<PencilIcon class="size-3.5" />
							</button>
							<button
								type="button"
								class="grid size-7 shrink-0 place-items-center rounded-md border hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50"
								title="Use camera-reported manufacturer"
								aria-label="Use camera-reported manufacturer"
								disabled={updatingManufacturer}
								onclick={() => void saveManufacturer(null)}
							>
								<RotateCcwIcon class="size-3.5" />
							</button>
						{/if}
					</dd>
					{#if manufacturerError}
						<dt></dt>
						<dd class="text-right text-destructive" role="alert">{manufacturerError}</dd>
					{/if}
					<dt class="text-muted-foreground">Model</dt>
					<dd class="text-right">{camera.model ?? 'Unknown'}</dd>
					<dt class="text-muted-foreground">Firmware</dt>
					<dd class="text-right font-mono">{camera.firmware_version ?? 'Unknown'}</dd>
					<dt class="text-muted-foreground">Serial</dt>
					<dd class="text-right font-mono">{camera.serial_number ?? 'Unknown'}</dd>
					<dt class="text-muted-foreground">Hardware</dt>
					<dd class="text-right font-mono">{camera.hardware_id ?? 'Unknown'}</dd>
					<dt class="text-muted-foreground">Hostname</dt>
					<dd class="text-right">{camera.hostname ?? 'Unknown'}</dd>
					<dt class="text-muted-foreground">MAC address</dt>
					<dd class="text-right font-mono">{camera.mac_address ?? 'Unknown'}</dd>
				</dl>
			</div>
			<div class="border-t pt-5 lg:border-t-0 lg:border-l lg:pt-0 lg:pl-8">
				<h2 class="mb-3 text-sm font-semibold">Connection</h2>
				<dl class="grid grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)] gap-x-5 gap-y-2 text-xs">
					<dt class="text-muted-foreground">Backend</dt>
					<dd class="text-right font-mono">{camera.backend ?? 'Unknown'}</dd>
					<dt class="text-muted-foreground">Configured transport</dt>
					<dd class="text-right font-mono uppercase">{camera.transport ?? 'Unknown'}</dd>
					<dt class="text-muted-foreground">Known service ports</dt>
					<dd class="text-right font-mono">{portSummary()}</dd>
					<dt class="text-muted-foreground">Last stream report</dt>
					<dd class="text-right font-mono">
						{liveHealth?.streams.length
							? formatAge(Math.max(...liveHealth.streams.map((stream) => stream.report_age_ms)))
							: 'Unavailable'}
					</dd>
				</dl>
				{#if liveHealth?.last_error}
					<p
						class="mt-4 border-l-2 border-amber-500 px-3 py-1.5 text-xs text-amber-800 dark:text-amber-300"
					>
						{liveHealth.last_error}
					</p>
				{/if}
			</div>
		</section>

		<section class="border-y py-5" aria-labelledby="motion-heading">
			<div class="flex flex-wrap items-center justify-between gap-4">
				<div>
					<h2 id="motion-heading" class="text-sm font-semibold">Motion detection</h2>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{motionSummary(details.motion_detection)}
					</p>
				</div>
				{#if details.motion_detection.controllable}
					<label class="flex items-center gap-2 text-sm font-medium">
						<input
							type="checkbox"
							role="switch"
							class="size-4 accent-emerald-600 disabled:cursor-not-allowed"
							checked={details.motion_detection.enabled ?? false}
							disabled={updatingMotion || details.motion_detection.enabled === null}
							onchange={handleMotionChange}
						/>
						<span>{updatingMotion ? 'Updating' : 'Enabled'}</span>
					</label>
				{/if}
			</div>
			{#if details.motion_detection.error || motionError}
				<p
					class="mt-3 border-l-2 border-destructive px-3 py-1.5 text-xs text-destructive"
					role="alert"
				>
					{motionError ?? details.motion_detection.error}
				</p>
			{/if}
		</section>

		<section class="border-y py-5" aria-labelledby="profiles-heading">
			<div class="mb-3 flex flex-wrap items-end justify-between gap-3">
				<div>
					<h2 id="profiles-heading" class="text-sm font-semibold">Configured media profiles</h2>
					<p class="mt-0.5 text-xs text-muted-foreground">
						Reported by the camera during discovery
					</p>
				</div>
				<span class="font-mono text-xs text-muted-foreground"
					>{camera.profiles.length} profiles</span
				>
			</div>
			<div class="overflow-x-auto border-y">
				<table class="w-full min-w-[58rem] text-left text-xs">
					<thead class="bg-muted/40 text-[10px] text-muted-foreground uppercase">
						<tr>
							<th class="px-3 py-2 font-semibold">Profile</th>
							<th class="px-3 py-2 font-semibold">Video</th>
							<th class="px-3 py-2 font-semibold">Rate</th>
							<th class="px-3 py-2 font-semibold">Bitrate / GOP</th>
							<th class="px-3 py-2 font-semibold">Audio</th>
						</tr>
					</thead>
					<tbody class="divide-y">
						{#each camera.profiles as profile (`${profile.stream}-${profile.name}`)}
							<tr>
								<td class="px-3 py-2.5 font-medium capitalize">{profile.stream}</td>
								<td class="px-3 py-2.5"
									><span class="font-mono uppercase">{profile.encoding ?? 'Unknown'}</span> · {profile.resolution ??
										'—'}
									{#if profile.h264_profile}
										<p class="text-[10px] text-muted-foreground">Profile {profile.h264_profile}</p>
									{/if}
								</td>
								<td class="px-3 py-2.5 font-mono"
									>{profile.framerate
										? `${numberFormatter.format(profile.framerate)} fps`
										: '—'}</td
								>
								<td class="px-3 py-2.5 font-mono"
									>{formatBitrate(profile.bitrate_kbps)} · GOP {profile.gop ?? '—'}</td
								>
								<td class="px-3 py-2.5 font-mono">
									{#if profile.audio}
										{profile.audio.encoding}
										{#if profile.audio.sample_rate}
											· {profile.audio.sample_rate} Hz
										{/if}
										· {formatBitrate(profile.audio.bitrate_kbps)}
									{:else}
										—
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		</section>

		<section class="border-y py-5" aria-labelledby="live-streams-heading">
			<div class="mb-3 flex flex-wrap items-end justify-between gap-3">
				<div>
					<h2 id="live-streams-heading" class="text-sm font-semibold">Live stream health</h2>
					<p class="mt-0.5 text-xs text-muted-foreground">Current backend ingress observations</p>
				</div>
				<span class="font-mono text-xs text-muted-foreground"
					>{liveHealth?.streams.length ?? 0} streams</span
				>
			</div>
			{#if liveHealth?.streams.length}
				<div class="overflow-x-auto border-y">
					<table class="w-full min-w-[82rem] text-left text-xs">
						<thead class="bg-muted/40 text-[10px] text-muted-foreground uppercase">
							<tr>
								<th class="px-3 py-2 font-semibold">Stream</th>
								<th class="px-3 py-2 font-semibold">Format</th>
								<th class="px-3 py-2 font-semibold">FPS</th>
								<th class="px-3 py-2 font-semibold">Bitrate</th>
								<th class="px-3 py-2 font-semibold">Max frame</th>
								<th class="px-3 py-2 font-semibold">Frames / bytes</th>
								<th class="px-3 py-2 font-semibold">Keyframes</th>
								<th class="px-3 py-2 font-semibold">Gap / jitter</th>
								<th class="px-3 py-2 font-semibold">Drops / errors</th>
								<th class="px-3 py-2 font-semibold">Reconnects</th>
								<th class="px-3 py-2 font-semibold">Age</th>
							</tr>
						</thead>
						<tbody class="divide-y">
							{#each liveHealth.streams as stream (`${stream.type}-${stream.updated_at_ms}`)}
								<tr>
									<td class="px-3 py-2.5 font-medium">{streamLabel(stream)}</td>
									<td class="px-3 py-2.5"
										><span class="font-mono uppercase">{stream.codec ?? '—'}</span>
										<p class="text-[10px] text-muted-foreground">{stream.resolution ?? '—'}</p></td
									>
									<td class="px-3 py-2.5 font-mono">
										{#if isAudioStream(stream)}
											{numberFormatter.format(stream.fps ?? 0)}
										{:else}
											{numberFormatter.format(stream.fps ?? 0)} / {numberFormatter.format(
												stream.expected_fps ?? 0
											)}
										{/if}
									</td>
									<td class="px-3 py-2.5 font-mono">{formatBitrate(stream.kbps)}</td>
									<td class="px-3 py-2.5 font-mono">{formatFrameSize(stream.max_frame_kb)}</td>
									<td class="px-3 py-2.5 font-mono"
										>{compactFormatter.format(stream.frames ?? 0)} · {formatBytes(stream.bytes)}</td
									>
									<td class="px-3 py-2.5 font-mono">
										{#if isAudioStream(stream)}
											<span class="text-muted-foreground">N/A</span>
										{:else}
											{compactFormatter.format(stream.keyframes ?? 0)}
											<p class="text-[10px] text-muted-foreground">
												{numberFormatter.format(stream.kf_fps ?? 0)}/s
											</p>
										{/if}
									</td>
									<td class="px-3 py-2.5 font-mono">
										{#if isAudioStream(stream)}
											<span class="text-muted-foreground">N/A</span>
										{:else}
											{numberFormatter.format(stream.gap_avg_ms ?? 0)} ms
											<p class="text-[10px] text-muted-foreground">
												max {numberFormatter.format(stream.gap_max_ms ?? 0)} ms · p99 {numberFormatter.format(
													stream.jitter_p99_ms ?? 0
												)} ms
											</p>
										{/if}
									</td>
									<td class="px-3 py-2.5 font-mono">
										{#if isAudioStream(stream)}
											<span class="text-muted-foreground">N/A</span>
										{:else}
											{compactFormatter.format(stream.drops ?? 0)} / {compactFormatter.format(
												stream.errors ?? 0
											)}
										{/if}
									</td>
									<td class="px-3 py-2.5 font-mono">
										{#if isAudioStream(stream)}
											<span class="text-muted-foreground">N/A</span>
										{:else}
											{compactFormatter.format(stream.reconnects ?? 0)}
										{/if}
									</td>
									<td class="px-3 py-2.5 font-mono">{formatAge(stream.report_age_ms)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{:else}
				<p class="border-y px-3 py-4 text-sm text-muted-foreground">
					No live stream health has been reported.
				</p>
			{/if}
		</section>

		<section class="border-y py-5" aria-labelledby="capabilities-heading">
			<h2 id="capabilities-heading" class="mb-3 text-sm font-semibold">Reported capabilities</h2>
			{#if capabilityItems.length}
				<div class="flex flex-wrap gap-2">
					{#each capabilityItems as [label, supported] (label)}
						<span
							class="border px-2 py-1 text-xs {supported
								? 'border-emerald-500/35 text-emerald-700 dark:text-emerald-300'
								: 'text-muted-foreground'}">{label}: {supported ? 'yes' : 'no'}</span
						>
					{/each}
				</div>
			{:else}
				<p class="text-sm text-muted-foreground">Capabilities were not reported by this camera.</p>
			{/if}
		</section>
	{/if}
</div>
