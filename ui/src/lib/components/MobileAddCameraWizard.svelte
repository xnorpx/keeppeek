<script lang="ts">
	import type { CameraWizardDraft } from '$lib/camera-wizard';
	import type { DiscoveredCameraSettings } from '$lib/types';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';
	import LockIcon from '@lucide/svelte/icons/lock';
	import RadioIcon from '@lucide/svelte/icons/radio';

	export type MobileCameraWizardStage = 'find-connect' | 'review' | 'streams';

	type Props = {
		stage: MobileCameraWizardStage;
		draft: CameraWizardDraft;
		discovered: readonly DiscoveredCameraSettings[];
		subnetPrefixes: string;
		manualAddress: string;
		discovering: boolean;
		discoveryElapsedMs: number;
		discoveryAttempted: boolean;
		error: string | null;
		saving: boolean;
		actionFixed?: boolean;
		oncancel?: () => void;
		ondiscover?: () => void | Promise<void>;
		onselect?: (camera: DiscoveredCameraSettings) => void;
		onsubnets?: (value: string) => void;
		onmanualaddress?: (value: string) => void;
		onupdate?: (update: Partial<CameraWizardDraft>) => void;
		onconnect?: () => void;
		onreview?: () => void;
		onsave?: () => void | Promise<void>;
	};

	let {
		stage,
		draft,
		discovered,
		subnetPrefixes,
		manualAddress,
		discovering,
		discoveryElapsedMs,
		discoveryAttempted,
		error,
		saving,
		actionFixed = true,
		oncancel,
		ondiscover,
		onselect,
		onsubnets,
		onmanualaddress,
		onupdate,
		onconnect,
		onreview,
		onsave
	}: Props = $props();

	const stageDetails: Record<MobileCameraWizardStage, { label: string; number: number }> = {
		'find-connect': { label: 'Add a camera', number: 1 },
		streams: { label: 'Prove the streams', number: 2 },
		review: { label: 'Review & save', number: 3 }
	};

	function evidenceLabel(camera: DiscoveredCameraSettings): string {
		return [camera.brand, camera.model, ...camera.sources]
			.filter((value): value is string => Boolean(value))
			.join(' · ');
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
		<div class="flex h-[660px] shrink-0 flex-col gap-[14px] overflow-hidden p-4">
			<div class="h-[49px] shrink-0">
				<h2 class="text-xl leading-6 font-semibold">Find and connect</h2>
				<p class="mt-[5px] text-xs leading-5 text-text-muted">
					Scanning uses a five-second window. Nothing is saved yet.
				</p>
			</div>

			<button
				type="button"
				class="flex h-12 shrink-0 flex-col gap-2 rounded-sm border border-hairline bg-raised px-3 py-2 text-left disabled:opacity-60"
				disabled={discovering}
				onclick={() => void ondiscover?.()}
				aria-label="Scan this network"
			>
				<span
					class="flex w-full items-center justify-between font-mono text-2xs leading-3 text-text-faint"
				>
					<span>{subnetPrefixes}.0/24</span>
					<span
						>{discovering
							? `${Math.max(0, (5_000 - discoveryElapsedMs) / 1_000).toFixed(1)}s`
							: 'SCAN'}</span
					>
				</span>
				<span class="h-[3px] w-full overflow-hidden rounded-full bg-hairline">
					<span
						class="block h-full bg-primary"
						style:width={`${discovering ? Math.min(100, (discoveryElapsedMs / 5_000) * 100) : discoveryAttempted ? 100 : 0}%`}
					></span>
				</span>
			</button>

			<p class="h-3 shrink-0 font-mono text-2xs leading-3 text-text-faint uppercase">
				{discovered.length} found
			</p>
			<div class="flex h-[130px] shrink-0 flex-col gap-[14px]">
				{#each discovered.slice(0, 2) as camera (camera.ip)}
					<button
						type="button"
						class="flex h-[58px] shrink-0 items-center gap-[11px] rounded-sm border px-[14px] text-left disabled:opacity-45 {draft.ip ===
						camera.ip
							? 'border-primary bg-primary/5'
							: 'border-hairline bg-surface'}"
						disabled={camera.configured}
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
								{camera.configured ? 'ALREADY ADDED' : evidenceLabel(camera)}
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

			<div
				class="flex h-[116px] shrink-0 flex-col gap-2.5 rounded-md border border-hairline bg-surface p-[14px]"
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
							placeholder="Username"
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
							placeholder="Password"
							autocomplete="new-password"
							oninput={(event) => onupdate?.({ password: event.currentTarget.value })}
						/>
					</label>
				</div>
				<p class="text-xs leading-4 text-primary-soft">Use a different login for this camera</p>
			</div>
			{#if error}<p class="text-xs leading-4 text-live-text" role="alert">{error}</p>{/if}
		</div>
	{:else if stage === 'streams'}
		<div class="flex h-[660px] shrink-0 flex-col gap-[14px] overflow-hidden p-4">
			<p class="h-10 shrink-0 text-xs leading-5 text-text-muted">
				Candidate stream probing is unavailable before save. Assign declared roles before review.
			</p>
			<div class="h-[250px] shrink-0 overflow-hidden rounded-md border border-primary bg-surface">
				<div class="grid h-[170px] place-items-center bg-video">
					<div class="text-center">
						<RadioIcon class="mx-auto size-5 text-text-faint" />
						<p class="mt-2 font-mono text-2xs leading-3 text-text-faint">PROBE UNAVAILABLE</p>
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
					><RadioIcon class="size-4 text-text-faint" /></span
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
			<div
				class="flex h-[62px] shrink-0 gap-3 rounded-sm border border-activity/40 bg-activity/10 p-3"
			>
				<span class="font-mono text-2xs leading-6 text-activity">AUDIO</span>
				<p class="text-xs-plus leading-[18px] text-text-muted">
					Codec evidence is unavailable until the saved camera publishes a stream.
				</p>
			</div>
			{#if error}<p class="text-xs leading-4 text-live-text" role="alert">{error}</p>{/if}
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
				{#each [['Address', draft.ip], ['Sign-in', draft.username ? 'Credentials provided' : 'Missing'], ['Record', draft.mainRtspUrl || 'Automatic main'], ['Live', draft.subRtspUrl || 'Automatic sub'], ['Retention', 'Server default']] as item (item[0])}
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
					The server does not estimate this unsaved camera's recording cost.
				</p>
			</div>
			<div
				class="flex h-[42px] shrink-0 items-center gap-2 rounded-sm border border-activity/35 bg-activity/10 px-3 text-xs leading-4 text-text-muted"
			>
				<span class="size-1.5 shrink-0 rounded-full bg-activity"></span>
				Connection and stream proof unavailable before save.
			</div>
			<p class="h-[18px] shrink-0 text-xs-plus leading-[18px] text-text-faint">
				Saving is the first configuration write.
			</p>
			{#if error}<p class="text-xs leading-4 text-live-text" role="alert">{error}</p>{/if}
		</div>
	{/if}

	<footer
		data-mobile-wizard-actions
		class="{actionFixed
			? 'fixed inset-x-0 bottom-0 z-50'
			: 'relative'} flex h-[68px] shrink-0 items-start gap-2 border-t border-hairline bg-surface px-4 pt-2.5 pb-5 md:hidden"
	>
		{#if stage === 'find-connect'}
			<input
				class="h-9 min-w-0 flex-1 bg-transparent text-xs-plus leading-4 outline-none placeholder:text-text-faint"
				value={manualAddress}
				placeholder="Enter an address"
				aria-label="Address or RTSP URL"
				oninput={(event) => onmanualaddress?.(event.currentTarget.value)}
			/>
			<button
				type="button"
				class="h-9 rounded-sm bg-primary px-4 text-md leading-[18px] font-semibold text-on-primary"
				onclick={onconnect}>Connect</button
			>
		{:else if stage === 'streams'}
			<button
				type="button"
				class="h-9 text-xs-plus leading-4 text-text-faint"
				disabled
				title="Candidate stream probe is unavailable">Re-probe unavailable</button
			>
			<button
				type="button"
				class="ml-auto h-9 rounded-sm bg-primary px-4 text-md leading-[18px] font-semibold text-on-primary"
				onclick={onreview}>Review</button
			>
		{:else}
			<button
				type="button"
				class="h-[38px] w-full rounded-sm bg-primary text-md leading-[18px] font-semibold text-on-primary disabled:opacity-45"
				disabled={saving}
				onclick={() => void onsave?.()}
			>
				{saving ? 'Saving camera' : 'Save camera'}
			</button>
		{/if}
	</footer>
</section>
