<script lang="ts">
	import { cameraDefaultsEvidence } from '$lib/camera-defaults';
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import type { CameraSettings, SanitizedConfig } from '$lib/types';

	type Props = {
		cameras: readonly CameraSettings[];
		config: SanitizedConfig;
	};

	let { cameras, config }: Props = $props();
	let evidence = $derived(cameraDefaultsEvidence(cameras));
	let visibleCameras = $derived(cameras.slice(0, 3));

	function cameraName(camera: CameraSettings): string {
		return camera.display_name?.trim() || camera.model?.trim() || camera.ip;
	}

	function credentialLabel(camera: CameraSettings): string {
		if (camera.username_configured && camera.password_configured) return 'Credentials configured';
		if (camera.username_configured || camera.password_configured) return 'Partial credentials';
		return 'No credentials configured';
	}

	function protocolLabel(): string {
		const used = Object.entries(evidence.backends).filter(([, count]) => count > 0);
		if (used.length !== 1) return 'Mixed';
		const backend = used[0][0];
		return backend === 'auto' ? 'Auto' : backend === 'reo-proto' ? 'Reo-Proto' : 'Retina';
	}

	function transportLabel(): string {
		const used = Object.entries(evidence.transports).filter(([, count]) => count > 0);
		return used.length === 1 ? used[0][0].toUpperCase() : 'Mixed';
	}

	function recordingLabel(): string {
		const used = Object.entries(evidence.recordingModes).filter(([, count]) => count > 0);
		if (used.length !== 1) return 'Mixed';
		if (used[0][0] === 'off') return 'Off';
		if (used[0][0] === 'event-boost') return 'Sub, main on events';
		if (used[0][0] === 'both') return 'Main + sub';
		return `${used[0][0]} only`;
	}
</script>

<section
	data-mobile-camera-defaults
	class="flex h-[660px] flex-col gap-[14px] overflow-hidden p-4 md:hidden"
	aria-label="Mobile camera defaults"
>
	<div class="flex h-6 shrink-0 items-baseline justify-between">
		<h2 class="text-xl leading-6 font-semibold">Shared login</h2>
		<span class="font-mono text-2xs leading-3 text-text-faint uppercase">
			{evidence.credentials.complete} of {evidence.cameraCount}
		</span>
	</div>

	<div
		class="flex h-[194px] shrink-0 flex-col gap-2.5 rounded-md border border-primary bg-raised p-[14px]"
	>
		<div class="flex h-[55px] shrink-0 flex-col gap-[5px]">
			<p class="font-mono text-2xs leading-3 text-text-faint">USERNAME</p>
			<div
				class="flex h-[38px] items-center rounded-sm border border-hairline-strong bg-surface px-[11px] font-mono text-sm leading-4 text-text-muted"
			>
				Not returned by the API
			</div>
		</div>
		<div class="flex h-[55px] shrink-0 flex-col gap-[5px]">
			<p class="font-mono text-2xs leading-3 text-text-faint">PASSWORD</p>
			<div
				class="flex h-[38px] items-center justify-between rounded-sm border border-hairline-strong bg-surface px-[11px] font-mono text-sm leading-4 text-text-muted"
			>
				<span>Write-only per camera</span><span>—</span>
			</div>
		</div>
		<CapabilityGate
			action="Change password"
			capability="keeppeek.runtime-config.v1"
			class="h-[34px] min-h-0 self-start"
		/>
	</div>

	<h3 class="h-5 shrink-0 text-lg leading-5 font-semibold">Other defaults</h3>
	<dl class="h-[146px] shrink-0 overflow-hidden rounded-md border border-hairline bg-surface">
		<div class="flex h-12 items-center justify-between border-b border-hairline px-[14px]">
			<dt class="text-sm leading-4">Protocol</dt>
			<dd class="font-mono text-xs leading-[14px] text-text-faint">{protocolLabel()}</dd>
		</div>
		<div class="flex h-12 items-center justify-between border-b border-hairline px-[14px]">
			<dt class="text-sm leading-4">Transport</dt>
			<dd class="font-mono text-xs leading-[14px] text-text-faint">{transportLabel()}</dd>
		</div>
		<div class="flex h-12 items-center justify-between px-[14px]">
			<dt class="text-sm leading-4">Recording</dt>
			<dd class="font-mono text-xs leading-[14px] text-text-faint">{recordingLabel()}</dd>
		</div>
	</dl>

	<div class="flex h-5 shrink-0 items-center justify-between">
		<h3 class="text-lg leading-5 font-semibold">Own login</h3>
		<span class="font-mono text-2xs leading-3 text-text-faint uppercase">
			{visibleCameras.length} cameras
		</span>
	</div>
	<ul class="h-[146px] shrink-0 overflow-hidden rounded-md border border-hairline bg-surface">
		{#each visibleCameras as camera (camera.id)}
			<li
				class="flex h-12 items-center justify-between border-b border-hairline px-[14px] last:border-b-0"
			>
				<div class="min-w-0">
					<p class="truncate text-sm leading-4">{cameraName(camera)}</p>
					<p class="truncate font-mono text-2xs leading-3 text-text-faint">
						{credentialLabel(camera)}
					</p>
				</div>
				<span class="text-sm leading-4 text-text-faint">—</span>
			</li>
		{/each}
	</ul>
	<span class="sr-only"
		>Retention estimate {config.recording_estimate.estimated_retention_days ?? 'unavailable'} days</span
	>
</section>
