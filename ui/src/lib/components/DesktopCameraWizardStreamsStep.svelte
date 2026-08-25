<script lang="ts">
	import type { CameraWizardDraft } from '$lib/camera-wizard';
	import type { CameraCatalogStreamHints, CameraStreamVerification } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';

	type Props = {
		draft: CameraWizardDraft;
		streamHints?: CameraCatalogStreamHints | null;
		catalogStreamsApplied?: boolean;
		streamResolution?: 'unresolved' | 'catalog' | 'probing' | 'onvif' | 'manual';
		streamProbeMessage?: string | null;
		streamEvidence?: readonly CameraStreamVerification[];
		verifying?: boolean;
		paperFrame?: boolean;
		onapplycatalogstreams?: () => void;
		onverify?: () => void;
		onupdate?: (update: Partial<CameraWizardDraft>) => void;
	};

	let {
		draft,
		streamHints = null,
		catalogStreamsApplied = false,
		streamResolution = 'unresolved',
		streamProbeMessage = null,
		streamEvidence = [],
		verifying = false,
		paperFrame = false,
		onapplycatalogstreams,
		onverify,
		onupdate
	}: Props = $props();
	const streams = [
		{ key: 'mainRtspUrl', label: 'Main stream', purpose: 'Recording stream', role: 'Main' },
		{ key: 'subRtspUrl', label: 'Sub stream', purpose: 'Live stream', role: 'Sub' }
	] as const;

	function evidenceFor(stream: 'Main' | 'Sub'): CameraStreamVerification | null {
		return (
			streamEvidence.find((evidence) => evidence.stream === stream.toLocaleLowerCase()) ?? null
		);
	}
</script>

{#if paperFrame}
	<div data-camera-wizard-streams class="flex h-[405px] w-[1140px] shrink-0 gap-5 px-7 py-6">
		{#each streams as stream (stream.key)}
			{@const evidence = evidenceFor(stream.role)}
			<article
				data-camera-wizard-stream={stream.role.toLowerCase()}
				class="flex h-[357px] w-[532px] shrink-0 flex-col overflow-hidden rounded-md border border-hairline-strong bg-raised"
			>
				<div class="flex h-[220px] shrink-0 flex-col justify-between bg-raised p-3">
					<div class="flex items-center justify-between">
						<span
							class="inline-flex h-[22px] items-center gap-[7px] rounded-xs bg-ground/75 px-[9px] font-mono text-[10px] leading-3 tracking-[0.1em] text-text-muted"
						>
							<span class="size-1.5 rounded-full bg-healthy"></span> AUTHENTICATED MEDIA
						</span>
						<span
							class="inline-flex h-[22px] items-center rounded-xs bg-ground/75 px-[9px] font-mono text-[10px] leading-3 tracking-[0.08em] text-text-faint"
						>
							{evidence?.codec?.toUpperCase() ?? 'CODEC NOT REPORTED'} · {evidence?.resolution ??
								'SIZE NOT REPORTED'}
						</span>
					</div>
					<div class="grid place-items-center">
						{#if evidence?.verified}<CheckIcon class="size-6 text-healthy" />{:else}<RadioIcon
								class="size-6 text-text-faint"
							/>{/if}
					</div>
					<div
						class="flex items-center justify-between font-mono text-[10px] leading-3 tracking-[0.08em] text-text-faint"
					>
						<span>{evidence?.frames_received ?? 0} FRAMES · {evidence?.elapsed_ms ?? 0}MS</span
						><span>{evidence?.keyframe_received ? 'FIRST KEYFRAME' : 'NO KEYFRAME'}</span>
					</div>
				</div>

				<div class="flex h-[70px] shrink-0 items-center justify-between px-4 pt-3.5 pb-2.5">
					<div class="flex min-w-0 flex-col gap-[3px]">
						<h3 class="text-base leading-5 font-semibold">{stream.label}</h3>
						<p class="max-w-[390px] truncate font-mono text-2xs leading-[14px] text-text-faint">
							{draft[stream.key] || 'No explicit RTSP URL'}
						</p>
					</div>
					<span class="font-mono text-2xs leading-[14px] text-activity"
						>{evidence?.verified ? 'VERIFIED' : 'NOT VERIFIED'}</span
					>
				</div>

				<div class="flex h-[67px] shrink-0 flex-col gap-2 px-4 pt-1.5 pb-4">
					<p class="font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-faint">ROLE</p>
					<div class="flex gap-2">
						<span
							class="inline-flex h-[30px] items-center rounded-sm bg-primary px-3 text-[13px] font-semibold text-on-primary"
						>
							{stream.purpose}
						</span>
						<span
							class="inline-flex h-[30px] items-center rounded-sm border border-hairline-strong px-3 text-[13px] text-text-muted"
						>
							{stream.role} declaration
						</span>
					</div>
				</div>
			</article>
		{/each}
	</div>

	<div data-stream-probe-notice class="flex h-[76px] w-[1140px] shrink-0 px-7 pb-1">
		<div
			class="flex h-[72px] w-[1084px] items-start gap-3 rounded-md border border-activity/40 bg-activity/10 px-4 py-3.5"
		>
			<CheckIcon class="mt-0.5 size-4 shrink-0 text-healthy" />
			<div class="flex flex-col gap-[3px]">
				<p class="text-sm leading-[18px] font-semibold">Main and sub video + keyframe verified</p>
				<p class="text-[13px] leading-[21px] text-text-muted">
					KeepPeek authenticated, described each endpoint, and received a video keyframe before the
					configuration write.
				</p>
			</div>
		</div>
	</div>
{:else}
	<div class="mx-auto max-w-3xl space-y-4">
		<div>
			<h3 class="text-sm font-semibold">Stream roles</h3>
			<p class="mt-1 text-xs leading-5 text-text-muted">
				Explicit URLs are optional when the server can derive streams after saving.
			</p>
		</div>
		{#if streamHints && (streamHints.main_rtsp_url || streamHints.sub_rtsp_url)}
			<div
				class="flex flex-wrap items-center justify-between gap-3 rounded-sm border border-primary/35 bg-primary/5 px-3 py-2.5"
			>
				<p class="text-xs leading-5 text-text-muted">
					Catalog stream references are credential-free, editable, and apply automatically. You can
					restore them after an ONVIF lookup.
				</p>
				<button
					type="button"
					class="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-sm border border-primary/60 px-3 text-xs font-medium text-primary-soft"
					aria-pressed={catalogStreamsApplied}
					onclick={onapplycatalogstreams}
				>
					{#if catalogStreamsApplied}<CheckIcon class="size-3.5" />{/if}
					{catalogStreamsApplied ? 'Catalog streams applied' : 'Restore catalog streams'}</button
				>
			</div>
		{/if}
		<div class="grid gap-3 md:grid-cols-2">
			{#each streams as stream (stream.key)}
				{@const evidence = evidenceFor(stream.role)}
				<label class="overflow-hidden rounded-md border border-hairline bg-raised">
					<span
						class="grid aspect-video place-items-center {evidence?.verified
							? 'bg-healthy/10'
							: 'bg-video'}"
					>
						{#if evidence?.verified}
							<span class="text-center">
								<CheckIcon class="mx-auto size-5 text-healthy" />
								<span class="mt-2 block font-mono text-2xs text-healthy">VIDEO + KEYFRAME</span>
								<span class="mt-1 block font-mono text-2xs text-text-muted">
									{evidence.codec?.toUpperCase()} · {evidence.resolution} · {evidence.frames_received}
									FRAMES · {evidence.elapsed_ms}MS
								</span>
							</span>
						{:else}
							<span class="text-center">
								<RadioIcon class="mx-auto size-5 text-text-faint" />
								<span class="mt-2 block font-mono text-2xs text-text-faint">
									{evidence?.error ?? 'NOT VERIFIED'}
								</span>
							</span>
						{/if}
					</span>
					<span class="block space-y-1.5 p-3 text-xs font-medium">
						{stream.purpose}
						<span class="block font-mono text-2xs tracking-caps text-text-faint">
							{stream.role} · {evidence?.verified ? 'VERIFIED' : 'NOT VERIFIED'}
						</span>
						<input
							class="mt-2 h-9 w-full rounded-sm border border-hairline-strong bg-surface px-3 font-mono text-2xs outline-none focus:border-ring focus:ring-1 focus:ring-ring"
							value={draft[stream.key]}
							placeholder="rtsp://…"
							oninput={(event) => onupdate?.({ [stream.key]: event.currentTarget.value })}
						/>
					</span>
				</label>
			{/each}
		</div>
		<div
			class="flex items-start gap-3 rounded-md border px-3 py-3 text-xs leading-5 {streamEvidence.some(
				(stream) => stream.verified
			)
				? 'border-healthy/40 bg-healthy/10'
				: 'border-activity bg-activity/10'}"
		>
			{#if streamEvidence.some((stream) => stream.verified)}
				<CheckIcon class="mt-0.5 size-4 shrink-0 text-healthy" />
			{:else}
				<TriangleAlertIcon class="mt-0.5 size-4 shrink-0 text-activity" />
			{/if}
			<span>
				<strong class="text-foreground"
					>{verifying || streamResolution === 'probing'
						? 'Authenticating and waiting for video keyframes.'
						: streamEvidence.some((stream) => stream.verified)
							? 'KeepPeek received authenticated video evidence.'
							: streamResolution === 'onvif'
								? 'ONVIF reported candidate RTSP endpoints.'
								: streamResolution === 'catalog'
									? 'Catalog candidate RTSP endpoints are applied.'
									: 'No candidate RTSP endpoint is available yet.'}</strong
				><br />{streamProbeMessage ??
					(verifying
						? 'Main and sub are checked in parallel. Editing either URL invalidates this evidence.'
						: 'Every stream required by the recording policy must report video and a keyframe before save.')}
			</span>
			<button
				type="button"
				class="ml-auto h-8 shrink-0 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium disabled:opacity-50"
				disabled={verifying}
				onclick={onverify}
			>
				{verifying ? 'Verifying' : 'Verify streams'}
			</button>
		</div>
	</div>
{/if}
