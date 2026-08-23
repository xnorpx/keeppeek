<script lang="ts">
	import type { CameraWizardDraft } from '$lib/camera-wizard';
	import type { CameraCatalogStreamHints } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';

	type Props = {
		draft: CameraWizardDraft;
		streamHints?: CameraCatalogStreamHints | null;
		catalogStreamsApplied?: boolean;
		streamResolution?: 'unresolved' | 'catalog' | 'probing' | 'onvif' | 'manual';
		streamProbeMessage?: string | null;
		paperFrame?: boolean;
		onapplycatalogstreams?: () => void;
		onupdate?: (update: Partial<CameraWizardDraft>) => void;
	};

	let {
		draft,
		streamHints = null,
		catalogStreamsApplied = false,
		streamResolution = 'unresolved',
		streamProbeMessage = null,
		paperFrame = false,
		onapplycatalogstreams,
		onupdate
	}: Props = $props();
	const streams = [
		{ key: 'mainRtspUrl', label: 'Main stream', purpose: 'Recording stream', role: 'Main' },
		{ key: 'subRtspUrl', label: 'Sub stream', purpose: 'Live stream', role: 'Sub' }
	] as const;
</script>

{#if paperFrame}
	<div data-camera-wizard-streams class="flex h-[405px] w-[1140px] shrink-0 gap-5 px-7 py-6">
		{#each streams as stream (stream.key)}
			<article
				data-camera-wizard-stream={stream.role.toLowerCase()}
				class="flex h-[357px] w-[532px] shrink-0 flex-col overflow-hidden rounded-md border border-hairline-strong bg-raised"
			>
				<div class="flex h-[220px] shrink-0 flex-col justify-between bg-raised p-3">
					<div class="flex items-center justify-between">
						<span
							class="inline-flex h-[22px] items-center gap-[7px] rounded-xs bg-ground/75 px-[9px] font-mono text-[10px] leading-3 tracking-[0.1em] text-text-muted"
						>
							<span class="size-1.5 rounded-full bg-activity"></span> ENDPOINT DECLARATION
						</span>
						<span
							class="inline-flex h-[22px] items-center rounded-xs bg-ground/75 px-[9px] font-mono text-[10px] leading-3 tracking-[0.08em] text-text-faint"
						>
							CODEC EVIDENCE UNAVAILABLE
						</span>
					</div>
					<div class="grid place-items-center">
						<RadioIcon class="size-6 text-text-faint" />
					</div>
					<div
						class="flex items-center justify-between font-mono text-[10px] leading-3 tracking-[0.08em] text-text-faint"
					>
						<span>NO FIRST-KEYFRAME OR DECODE TIMING</span><span>NOT DECODED</span>
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
						>{streamResolution === 'onvif' ? 'ONVIF REPORTED' : 'NOT TESTED'}</span
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
			<TriangleAlertIcon class="mt-0.5 size-4 shrink-0 text-activity" />
			<div class="flex flex-col gap-[3px]">
				<p class="text-sm leading-[18px] font-semibold">
					Decoded stream evidence is unavailable before save
				</p>
				<p class="text-[13px] leading-[21px] text-text-muted">
					Candidate endpoints can come from ONVIF or the catalog. URLs remain declarations until
					KeepPeek decodes the saved camera stream.
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
				<label class="overflow-hidden rounded-md border border-hairline bg-raised">
					<span class="grid aspect-video place-items-center bg-video">
						<RadioIcon class="size-5 text-text-faint" />
					</span>
					<span class="block space-y-1.5 p-3 text-xs font-medium">
						{stream.purpose}
						<span class="block font-mono text-2xs tracking-caps text-text-faint">
							{stream.role} · NOT PROBED
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
		<p
			class="flex items-start gap-2 rounded-md border border-activity bg-activity/10 px-3 py-3 text-xs leading-5 text-text-muted"
		>
			<TriangleAlertIcon class="mt-0.5 size-4 shrink-0 text-activity" />
			<span>
				<strong class="text-foreground"
					>{streamResolution === 'probing'
						? 'ONVIF lookup is in progress.'
						: streamResolution === 'onvif'
							? 'ONVIF reported candidate RTSP endpoints.'
							: streamResolution === 'catalog'
								? 'Catalog candidate RTSP endpoints are applied.'
								: 'No candidate RTSP endpoint is available yet.'}</strong
				><br />{streamProbeMessage ??
					(streamResolution === 'probing'
						? 'You can enter URLs manually now. Any ONVIF result will fill the remaining fields when it arrives.'
						: 'Both URL fields remain editable. KeepPeek validates decoded media only after the camera is saved.')}
			</span>
		</p>
	</div>
{/if}
