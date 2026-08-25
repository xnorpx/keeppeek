<script lang="ts">
	import type {
		CameraCatalogCamera,
		CameraCatalogInfo,
		CameraStreamProbeResult,
		CameraStreamVerification,
		ProfileSummary
	} from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import ScanSearchIcon from '@lucide/svelte/icons/scan-search';
	import CameraCatalogEvidence from './CameraCatalogEvidence.svelte';

	type Props = {
		catalogCamera?: CameraCatalogCamera | null;
		catalogInfo?: CameraCatalogInfo | null;
		probe?: CameraStreamProbeResult | null;
		compact?: boolean;
	};

	let { catalogCamera = null, catalogInfo = null, probe = null, compact = false }: Props = $props();
	let identity = $derived(
		[
			probe?.manufacturer,
			probe?.model,
			probe?.firmware_version && `Firmware ${probe.firmware_version}`,
			probe?.serial_number && `Serial ${probe.serial_number}`,
			probe?.hardware_id && `Hardware ${probe.hardware_id}`
		].filter((value): value is string => Boolean(value))
	);

	function profileLabel(profile: ProfileSummary): string {
		return [
			profile.stream.toUpperCase(),
			profile.encoding?.toUpperCase(),
			profile.resolution,
			profile.framerate === null ? null : `${profile.framerate} fps`,
			profile.bitrate_kbps === null ? null : `${profile.bitrate_kbps} kbps`,
			profile.gop === null ? null : `GOP ${profile.gop}`
		]
			.filter(Boolean)
			.join(' · ');
	}

	function verificationLabel(verification: CameraStreamVerification): string {
		if (!verification.verified) return verification.error ?? 'Not verified';
		return [
			verification.codec?.toUpperCase(),
			verification.resolution,
			verification.declared_fps === null ? null : `${verification.declared_fps} fps`,
			`${verification.frames_received} frame${verification.frames_received === 1 ? '' : 's'}`,
			verification.keyframe_received ? 'keyframe' : null,
			`${verification.elapsed_ms} ms`
		]
			.filter(Boolean)
			.join(' · ');
	}
</script>

<section
	data-camera-onboarding-evidence
	class="space-y-3"
	aria-labelledby="camera-onboarding-evidence-heading"
>
	<div class="flex items-center justify-between gap-3">
		<h3 id="camera-onboarding-evidence-heading" class="text-sm font-semibold">Camera evidence</h3>
		<span class="font-mono text-2xs tracking-caps text-text-faint">
			REFERENCE · REPORTED · MEASURED
		</span>
	</div>

	{#if catalogCamera}
		<div>
			<p class="mb-1.5 flex items-center gap-1.5 font-mono text-2xs tracking-caps text-text-faint">
				<DatabaseIcon class="size-3" /> DATABASE REFERENCE
			</p>
			<CameraCatalogEvidence camera={catalogCamera} {catalogInfo} {compact} />
		</div>
	{/if}

	<div class="border-y border-hairline bg-raised">
		<div class="flex items-start gap-3 px-3 py-3">
			<ScanSearchIcon class="mt-0.5 size-4 shrink-0 text-primary-soft" />
			<div class="min-w-0 flex-1">
				<div class="flex flex-wrap items-center justify-between gap-2">
					<p class="text-xs font-semibold">ONVIF report</p>
					<span class="font-mono text-2xs text-text-faint">CAMERA REPORTED</span>
				</div>
				{#if identity.length > 0}
					<p class="mt-1 text-xs leading-5 text-text-muted">{identity.join(' · ')}</p>
				{:else}
					<p class="mt-1 text-xs leading-5 text-text-muted">
						{probe?.onvif_error ?? 'No ONVIF identity was reported.'}
					</p>
				{/if}
				{#if !compact && probe?.profiles.length}
					<ul class="mt-2 divide-y divide-hairline border-t border-hairline">
						{#each probe.profiles as profile (`${profile.stream}-${profile.name}`)}
							<li class="flex min-h-8 items-center justify-between gap-3 py-1.5 text-xs">
								<span class="truncate font-medium">{profile.name}</span>
								<span class="truncate text-right font-mono text-2xs text-text-muted">
									{profileLabel(profile)}
								</span>
							</li>
						{/each}
					</ul>
				{/if}
			</div>
		</div>

		<div class="border-t border-hairline px-3 py-3">
			<div class="mb-2 flex items-center justify-between gap-2">
				<p class="flex items-center gap-1.5 text-xs font-semibold">
					<RadioIcon class="size-3.5 text-primary-soft" /> Live media proof
				</p>
				<span class="font-mono text-2xs text-text-faint">MEASURED BY KEEPPEEK</span>
			</div>
			<div class="divide-y divide-hairline">
				{#each probe?.streams ?? [] as stream (stream.stream)}
					<div class="flex min-h-8 items-center gap-2 py-1.5 text-xs">
						{#if stream.verified}<CheckIcon class="size-3.5 shrink-0 text-healthy" />{:else}<span
								class="size-2 shrink-0 rounded-full bg-live"
							></span>{/if}
						<span class="w-10 shrink-0 font-semibold capitalize">{stream.stream}</span>
						<span class="min-w-0 truncate font-mono text-2xs text-text-muted">
							{verificationLabel(stream)}
						</span>
					</div>
				{:else}
					<p class="py-1 text-xs text-text-muted">No live stream verification has completed.</p>
				{/each}
			</div>
		</div>
	</div>
</section>
