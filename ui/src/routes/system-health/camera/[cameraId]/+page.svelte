<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { useControlClient } from '$lib/control-context';
	import { useCapabilityState } from '$lib/capability-context';
	import DesktopCameraDiagnosis from '$lib/components/DesktopCameraDiagnosis.svelte';
	import MobileCameraDiagnosis from '$lib/components/MobileCameraDiagnosis.svelte';
	import { cameraDiagnosisEvidence } from '$lib/health-presentation';
	import type { ServerHealthResponse } from '$lib/types';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';

	const capabilities = useCapabilityState();
	const controlClient = useControlClient();
	let health = $state.raw<ServerHealthResponse | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let updatingTransport = $state(false);
	let updateError = $state<string | null>(null);
	let updateResult = $state<string | null>(null);
	let cameraId = $derived(page.params.cameraId ?? '');
	let evidence = $derived(health ? cameraDiagnosisEvidence(health, cameraId) : null);

	$effect(() => {
		const requestedCameraId = cameraId;
		const controller = new AbortController();
		health = null;
		loading = true;
		error = null;
		void loadHealth(requestedCameraId, controller.signal);
		return () => controller.abort();
	});

	async function loadHealth(requestedCameraId: string, signal?: AbortSignal): Promise<void> {
		try {
			const nextHealth = await controlClient.getHealth(signal);
			if (signal?.aborted || requestedCameraId !== cameraId) return;
			health = nextHealth;
			error = null;
		} catch (cause) {
			if (signal?.aborted || requestedCameraId !== cameraId) return;
			error = cause instanceof Error ? cause.message : 'Camera diagnosis is unavailable.';
		} finally {
			if (!signal?.aborted && requestedCameraId === cameraId) loading = false;
		}
	}

	async function switchToTcp(): Promise<void> {
		if (!evidence || updatingTransport || !evidence.canSuggestTcp) return;
		const commandId = `diagnosis-transport-${evidence.camera.id}`;
		if (!capabilities.begin(commandId, 'keeppeek.runtime-config.v1')) return;
		if (!capabilities.submit(commandId)) return;

		updatingTransport = true;
		updateError = null;
		updateResult = null;
		try {
			const result = await controlClient.updateCamera(evidence.camera.ip, { transport: 'tcp' });
			capabilities.succeed(commandId);
			if (health) {
				health = {
					...health,
					cameras: health.cameras.map((camera) =>
						camera.id === evidence?.camera.id
							? { ...camera, transport: result.camera.transport }
							: camera
					)
				};
			}
			updateResult = result.restart_required
				? 'Transport saved. Apply the pending restart to reconnect this camera on TCP.'
				: 'Transport changed to TCP.';
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Transport was not changed.';
			capabilities.fail(commandId, message);
			updateError = message;
		} finally {
			updatingTransport = false;
		}
	}
</script>

<svelte:head>
	<title>{evidence?.camera.name ?? 'Camera diagnosis'} - KeepPeek</title>
</svelte:head>

<div data-camera-diagnosis-page class="w-full">
	{#if loading}
		<div
			class="grid min-h-[32rem] place-items-center border-y text-sm text-text-muted"
			aria-label="Loading camera diagnosis"
		>
			<span class="flex items-center gap-2"
				><RefreshCwIcon class="size-4 animate-spin" /> Reading camera evidence</span
			>
		</div>
	{:else if error}
		<section
			class="grid min-h-80 place-items-center rounded-md border border-live/40 bg-live/5 p-6 text-center"
			role="alert"
		>
			<div class="max-w-lg space-y-4">
				<TriangleAlertIcon class="mx-auto size-7 text-live-text" />
				<div>
					<h1 class="text-lg font-semibold">Camera diagnosis is unavailable</h1>
					<p class="mt-1 text-sm break-words text-text-muted">{error}</p>
				</div>
				<a
					href={resolve('/system-health')}
					class="inline-flex h-9 items-center rounded-sm border border-hairline-strong bg-raised px-4 text-xs font-medium"
					>Back to Health</a
				>
			</div>
		</section>
	{:else if evidence}
		<div class="md:hidden">
			<MobileCameraDiagnosis
				{evidence}
				generatedAtMs={health?.generated_at_ms ?? Date.now()}
				{updatingTransport}
				statusMessage={updateResult}
				errorMessage={updateError}
				onswitchtotcp={switchToTcp}
			/>
		</div>
		<div class="hidden md:block">
			<DesktopCameraDiagnosis
				{evidence}
				runtimeConfigSupported={capabilities.supports('keeppeek.runtime-config.v1')}
				{updatingTransport}
				statusMessage={updateResult}
				errorMessage={updateError}
				onswitchtotcp={switchToTcp}
			/>
		</div>
	{:else}
		<section
			class="grid min-h-80 place-items-center rounded-md border border-hairline bg-surface p-6 text-center"
			role="status"
		>
			<div class="max-w-md space-y-3">
				<CameraIcon class="mx-auto size-7 text-text-faint" />
				<h1 class="text-lg font-semibold">Camera not found in this health snapshot</h1>
				<p class="text-sm text-text-muted">
					The camera may have been removed or its stable health ID changed.
				</p>
				<a
					href={resolve('/system-health')}
					class="inline-flex h-9 items-center rounded-sm border border-hairline-strong bg-raised px-4 text-xs font-medium"
					>Back to Health</a
				>
			</div>
		</section>
	{/if}
</div>
