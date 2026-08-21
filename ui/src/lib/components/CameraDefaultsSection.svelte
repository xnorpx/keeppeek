<script lang="ts">
	import { cameraDefaultsEvidence } from '$lib/camera-defaults';
	import type { CameraSettings, SanitizedConfig } from '$lib/types';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import LockIcon from '@lucide/svelte/icons/lock';
	import NetworkIcon from '@lucide/svelte/icons/network';
	import ShieldCheckIcon from '@lucide/svelte/icons/shield-check';

	type Props = {
		cameras: readonly CameraSettings[];
		config: SanitizedConfig;
	};

	let { cameras, config }: Props = $props();
	let evidence = $derived(cameraDefaultsEvidence(cameras));
	let visibleCameras = $derived(cameras.slice(0, 3));
	let hiddenCameraCount = $derived(Math.max(0, cameras.length - visibleCameras.length));

	function cameraName(camera: CameraSettings): string {
		return camera.display_name?.trim() || camera.model?.trim() || camera.ip;
	}

	function credentialLabel(camera: CameraSettings): string {
		if (camera.username_configured && camera.password_configured) {
			return 'Username + password configured';
		}
		if (camera.username_configured || camera.password_configured) {
			return 'Partial credentials';
		}
		return 'No credentials configured';
	}

	function retentionLabel(): string {
		const days = config.recording_estimate.estimated_retention_days;
		const estimate = days === null ? 'Estimate unavailable' : `About ${formatDays(days)}`;
		const cap =
			config.storage.long_term_max_gb === 0
				? 'no configured size cap'
				: `${config.storage.long_term_max_gb} GB cap`;
		return `${estimate} · ${cap}`;
	}

	function formatDays(days: number): string {
		if (days < 1) return `${Math.max(1, Math.round(days * 24))} hours`;
		return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(days)} days`;
	}
</script>

<section
	id="camera-defaults"
	class="flex h-[750px] scroll-mt-4 flex-col gap-[26px] overflow-hidden"
	aria-labelledby="camera-defaults-heading"
>
	<header class="flex h-[84px] shrink-0 items-end justify-between">
		<div class="w-[760px]">
			<h2 id="camera-defaults-heading" class="text-2xl leading-[34px] font-semibold">
				Camera defaults
			</h2>
			<p class="mt-1 text-sm leading-[22px] text-text-muted">
				The current server stores concrete settings on every camera. It does not expose a shared
				login, inheritance markers, or an override reset command.
			</p>
		</div>
		<div class="w-52 pb-1 text-right">
			<p class="text-3xl leading-[42px] font-semibold">{evidence.cameraCount}</p>
			<p class="font-mono text-2xs leading-[14px] tracking-caps text-text-faint">
				CAMERAS OBSERVED
			</p>
		</div>
	</header>

	<div
		class="grid h-[302px] shrink-0 grid-cols-[700px_minmax(0,1fr)] overflow-hidden rounded-md border border-primary bg-raised"
	>
		<div class="h-[300px] p-[22px]">
			<div class="flex h-6 items-center gap-2">
				<LockIcon class="size-4 text-primary-soft" />
				<h3 class="text-xl leading-6 font-semibold">Shared camera login</h3>
			</div>
			<div class="mt-[18px] grid h-[58px] grid-cols-[315px_325px] gap-[15px]">
				<div class="flex flex-col gap-1.5">
					<p class="font-mono text-2xs leading-[14px] tracking-caps text-text-faint">USERNAME</p>
					<p
						class="flex h-[38px] items-center rounded-sm border border-hairline-strong bg-surface px-3 text-sm leading-[18px] text-text-muted"
					>
						Not returned by the API
					</p>
				</div>
				<div class="flex flex-col gap-1.5">
					<p class="font-mono text-2xs leading-[14px] tracking-caps text-text-faint">PASSWORD</p>
					<p
						class="flex h-[38px] items-center justify-between rounded-sm border border-hairline-strong bg-surface px-3 text-sm leading-[18px] text-text-muted"
					>
						<span>Write-only per camera</span><span>—</span>
					</p>
				</div>
			</div>
			<div
				class="mt-4 h-[104px] rounded-sm border border-activity/45 bg-activity/5 px-3.5 py-3"
				role="status"
			>
				<div
					class="flex items-center gap-2 font-mono text-2xs leading-[14px] tracking-caps text-activity"
				>
					<CircleAlertIcon class="size-3.5" /> SHARED INHERITANCE NOT EXPOSED
				</div>
				<p class="mt-2 text-xs-plus leading-[18px] text-text-muted">
					Credential-presence booleans cannot prove that two cameras use the same secret. KeepPeek
					therefore cannot identify shared-login cameras or overrides from this response.
				</p>
			</div>
		</div>

		<div class="h-[300px] border-l border-hairline p-[22px]">
			<p class="font-mono text-2xs leading-[14px] tracking-caps text-text-faint">
				WHAT THE CURRENT API PROVES
			</p>
			<div class="mt-3 grid grid-cols-3 gap-2 text-center">
				<div
					class="flex h-[58px] flex-col justify-center rounded-sm border border-hairline bg-surface"
				>
					<p class="text-lg leading-5 font-semibold">{evidence.credentials.complete}</p>
					<p class="text-2xs leading-4 text-text-faint">Complete</p>
				</div>
				<div
					class="flex h-[58px] flex-col justify-center rounded-sm border border-hairline bg-surface"
				>
					<p class="text-lg leading-5 font-semibold">{evidence.credentials.partial}</p>
					<p class="text-2xs leading-4 text-text-faint">Partial</p>
				</div>
				<div
					class="flex h-[58px] flex-col justify-center rounded-sm border border-hairline bg-surface"
				>
					<p class="text-lg leading-5 font-semibold">{evidence.credentials.missing}</p>
					<p class="text-2xs leading-4 text-text-faint">Missing</p>
				</div>
			</div>
			<div class="mt-3 flex h-[60px] gap-2 rounded-sm bg-ground px-3 py-3">
				<ShieldCheckIcon class="mt-0.5 size-4 shrink-0 text-text-faint" />
				<p class="text-xs-plus leading-[18px] text-text-muted">
					Secrets are never returned. They may be replaced on an individual camera but cannot be
					read back or compared in the browser.
				</p>
			</div>
			<p class="mt-3 text-xs-plus leading-[18px] text-text-faint">
				No API reports which cameras inherit a credential or why a per-camera value differs.
			</p>
		</div>
	</div>

	<div class="grid h-[312px] shrink-0 grid-cols-[645px_645px] gap-5">
		<div class="h-[312px] rounded-md border border-hairline bg-surface p-5">
			<div class="flex h-[22px] items-center gap-2">
				<NetworkIcon class="size-4 text-text-muted" />
				<h3 class="text-xl leading-[22px] font-semibold">Observed fleet configuration</h3>
			</div>
			<dl class="mt-2.5 divide-y divide-hairline border-y border-hairline text-sm">
				<div class="flex h-11 items-center justify-between gap-2">
					<dt class="leading-[18px]">Protocol</dt>
					<dd class="font-mono text-xs leading-[14px] text-text-muted">
						Auto {evidence.backends.auto} · Retina {evidence.backends.retina} · Reo-Proto
						{evidence.backends['reo-proto']}
					</dd>
				</div>
				<div class="flex h-11 items-center justify-between gap-2">
					<dt class="leading-[18px]">Transport</dt>
					<dd class="font-mono text-xs leading-[14px] text-text-muted">
						TCP {evidence.transports.tcp} · UDP {evidence.transports.udp}
					</dd>
				</div>
				<div class="flex h-11 items-center justify-between gap-2">
					<dt class="leading-[18px]">Manual stream URLs</dt>
					<dd class="font-mono text-xs leading-[14px] text-text-muted">
						{evidence.manualStreamOverrides} cameras
					</dd>
				</div>
				<div class="flex h-11 items-center justify-between gap-2">
					<dt class="leading-[18px]">Recording mode</dt>
					<dd class="font-mono text-xs leading-[14px] text-text-faint">NOT EXPOSED PER CAMERA</dd>
				</div>
				<div class="flex h-11 items-center justify-between gap-2">
					<dt class="leading-[18px]">Retention reference</dt>
					<dd class="font-mono text-xs leading-[14px] text-text-muted">{retentionLabel()}</dd>
				</div>
			</dl>
			<p class="mt-2 flex h-5 gap-2 text-xs leading-5 text-text-faint">
				<DatabaseIcon class="mt-0.5 size-3.5 shrink-0" /> Retention is global storage evidence, not a
				camera inheritance value.
			</p>
		</div>

		<div class="h-[312px] rounded-md border border-hairline bg-surface p-5">
			<div class="flex h-[22px] items-center justify-between gap-3">
				<h3 class="flex items-center gap-2 text-xl leading-[22px] font-semibold">
					<CameraIcon class="size-4 text-text-muted" /> Per-camera credential evidence
				</h3>
				<span class="font-mono text-2xs leading-[14px] text-text-faint">INHERITANCE UNKNOWN</span>
			</div>
			{#if visibleCameras.length > 0}
				<ul class="mt-2.5 divide-y divide-hairline border-y border-hairline">
					{#each visibleCameras.slice(0, 3) as camera (camera.id)}
						<li class="flex h-[52px] items-center justify-between gap-3">
							<div class="min-w-0">
								<p class="truncate text-sm leading-[18px] font-medium">{cameraName(camera)}</p>
								<p class="truncate font-mono text-2xs leading-[14px] text-text-faint">
									{camera.ip}
								</p>
							</div>
							<div class="shrink-0 text-right">
								<p class="text-xs leading-4 text-text-muted">{credentialLabel(camera)}</p>
								<p class="font-mono text-2xs leading-[14px] text-text-faint">
									{camera.backend} · {camera.transport}
								</p>
							</div>
						</li>
					{/each}
				</ul>
				{#if hiddenCameraCount > 0}
					<p class="mt-2 font-mono text-2xs leading-[14px] text-text-faint">
						+ {hiddenCameraCount} MORE CAMERAS · OVERRIDE STATUS UNAVAILABLE
					</p>
				{/if}
			{:else}
				<p
					class="rounded-sm border border-dashed border-hairline-strong px-3 py-5 text-center text-sm text-text-muted"
				>
					No camera settings are available to summarize.
				</p>
			{/if}
		</div>
	</div>
</section>
