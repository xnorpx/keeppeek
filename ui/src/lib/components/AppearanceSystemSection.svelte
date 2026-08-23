<script lang="ts">
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { useAppearanceState } from '$lib/appearance-context';
	import { appearanceSystemEvidence, type ThemePreference } from '$lib/appearance-system';
	import type { CameraCatalogInfo, ServerHealthResponse } from '$lib/types';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import SunMoonIcon from '@lucide/svelte/icons/sun-moon';
	import AppearanceSystemPaperFrame from './AppearanceSystemPaperFrame.svelte';

	type Props = {
		health: ServerHealthResponse | null;
		healthError?: string | null;
		catalogInfo?: CameraCatalogInfo | null;
		restarting?: boolean;
		paperFrame?: boolean;
		timeZoneOverride?: string | null;
		reducedMotionOverride?: boolean | null;
		logLines?: readonly string[];
		logsLive?: boolean;
		logTarget?: string;
		onrestart: () => void;
	};

	let {
		health,
		healthError = null,
		catalogInfo = null,
		restarting = false,
		paperFrame = false,
		timeZoneOverride,
		reducedMotionOverride,
		logLines = [],
		logsLive = false,
		logTarget = 'all',
		onrestart
	}: Props = $props();
	const appearance = useAppearanceState();
	let browserTimeZone = $state<string | null>(null);
	let prefersReducedMotion = $state<boolean | null>(null);
	let evidence = $derived(appearanceSystemEvidence(health));

	onMount(() => {
		browserTimeZone =
			timeZoneOverride === undefined
				? Intl.DateTimeFormat().resolvedOptions().timeZone || null
				: timeZoneOverride;
		prefersReducedMotion =
			reducedMotionOverride === undefined
				? window.matchMedia('(prefers-reduced-motion: reduce)').matches
				: reducedMotionOverride;
	});

	function formatDuration(seconds: number | null): string {
		if (seconds === null) return 'Unavailable';
		const days = Math.floor(seconds / 86_400);
		const hours = Math.floor((seconds % 86_400) / 3_600);
		const minutes = Math.floor((seconds % 3_600) / 60);
		if (days > 0) return `${days}d ${hours}h ${minutes}m`;
		if (hours > 0) return `${hours}h ${minutes}m`;
		return `${minutes}m`;
	}

	const themeOptions: Array<{ value: ThemePreference; label: string }> = [
		{ value: 'dark', label: 'Dark' },
		{ value: 'light', label: 'Light' },
		{ value: 'system', label: 'Match system' }
	];
</script>

{#if paperFrame}
	<AppearanceSystemPaperFrame
		{health}
		{browserTimeZone}
		{prefersReducedMotion}
		{restarting}
		{logLines}
		{logsLive}
		{logTarget}
		{onrestart}
	/>
{:else}
	<section
		id="appearance"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
		aria-labelledby="appearance-system-heading"
	>
		<header
			class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">APPEARANCE · SYSTEM · LOGS</p>
				<h2 id="appearance-system-heading" class="mt-1 text-xl font-semibold">
					The last three settings sections
				</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">
					Theme is a browser preference. Runtime identity comes from server health. Restart and logs
					are implemented commands; every other system setting below remains evidence-only.
				</p>
			</div>
			{#if health}
				<span
					class="rounded-full border border-activity/45 bg-activity/5 px-3 py-1 font-mono text-2xs tracking-caps text-text-muted"
				>
					PRE-1.0 · {health.version}
				</span>
			{/if}
		</header>

		<div class="grid lg:grid-cols-3">
			<article class="space-y-4 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<h3 class="flex items-center gap-2 text-base font-semibold">
					<SunMoonIcon class="size-4" /> Appearance & time
				</h3>
				<div>
					<p class="font-mono text-2xs tracking-caps text-text-faint">THEME</p>
					<div
						class="mt-2 grid grid-cols-3 overflow-hidden rounded-sm border border-hairline-strong"
						role="group"
						aria-label="Theme preference"
					>
						{#each themeOptions as option (option.value)}
							<button
								type="button"
								class="h-9 border-r border-hairline text-xs last:border-r-0 {appearance.preference ===
								option.value
									? 'bg-primary font-semibold text-on-primary'
									: 'bg-raised text-text-muted'}"
								aria-pressed={appearance.preference === option.value}
								onclick={() => appearance.setPreference(option.value)}
							>
								{option.label}
							</button>
						{/each}
					</div>
					<p class="mt-2 text-xs text-text-faint">
						Effective theme: {appearance.effectiveTheme}. Video surfaces stay dark in both themes.
					</p>
				</div>

				<dl class="divide-y divide-hairline border-y border-hairline text-xs">
					<div class="py-3">
						<div class="flex justify-between gap-3">
							<dt class="text-text-muted">Server time zone</dt>
							<dd class="font-mono text-text-faint">Unavailable</dd>
						</div>
						<p class="mt-1 text-text-faint">
							Browser reports {browserTimeZone ?? 'unavailable'}; this is not server evidence.
						</p>
					</div>
					<div class="flex justify-between gap-3 py-3">
						<dt class="text-text-muted">Clock preference</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex justify-between gap-3 py-3">
						<dt class="text-text-muted">Week starts</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex justify-between gap-3 py-3">
						<dt class="text-text-muted">Language</dt>
						<dd class="font-mono">{evidence.language} only</dd>
					</div>
					<div class="flex justify-between gap-3 py-3">
						<dt class="text-text-muted">Browser reduced motion</dt>
						<dd class="font-mono">
							{prefersReducedMotion === null
								? 'Unavailable'
								: prefersReducedMotion
									? 'Reduce'
									: 'No preference'}
						</dd>
					</div>
				</dl>
				<p class="text-xs leading-5 text-text-faint">
					Timeline and export timezone formatting is not rewired by a cosmetic control; no
					unsupported preference is persisted.
				</p>
			</article>

			<article class="space-y-4 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<h3 class="flex items-center gap-2 text-base font-semibold">
					<ActivityIcon class="size-4" /> System & updates
				</h3>
				{#if healthError && !health}
					<p
						class="rounded-sm border border-activity/45 bg-activity/5 px-3 py-2.5 text-xs text-text-muted"
					>
						{healthError}
					</p>
				{/if}
				<dl class="divide-y divide-hairline border-y border-hairline text-xs">
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Version</dt>
						<dd class="font-mono">{evidence.system.version ?? 'Unavailable'}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Host</dt>
						<dd class="font-mono">{evidence.system.hostName ?? 'Unavailable'}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Operating system</dt>
						<dd class="font-mono">{evidence.system.operatingSystem ?? 'Unavailable'}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Process</dt>
						<dd class="font-mono">{evidence.system.processName ?? 'Unavailable'}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Uptime</dt>
						<dd class="font-mono">{formatDuration(evidence.system.uptimeSeconds)}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Update channel</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Config file</dt>
						<dd class="font-mono text-text-faint">Path unavailable</dd>
					</div>
					{#if catalogInfo}
						<div class="flex justify-between gap-3 py-2.5">
							<dt class="text-text-muted">Camera catalog</dt>
							<dd class="font-mono">{catalogInfo.version}</dd>
						</div>
						<div class="flex justify-between gap-3 py-2.5">
							<dt class="text-text-muted">Catalog cameras</dt>
							<dd class="font-mono">{catalogInfo.camera_count.toLocaleString()}</dd>
						</div>
						<div class="flex justify-between gap-3 py-2.5">
							<dt class="text-text-muted">Catalog generated</dt>
							<dd class="max-w-[65%] text-right font-mono break-all text-text-faint">
								{catalogInfo.generated_at}
							</dd>
						</div>
						<div class="flex justify-between gap-3 py-2.5">
							<dt class="text-text-muted">Catalog source</dt>
							<dd>
								<a
									href={catalogInfo.website_url}
									target="_blank"
									rel="noreferrer"
									class="inline-flex items-center gap-1 text-primary-soft hover:text-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								>
									CCTV Database <ExternalLinkIcon class="size-3" />
								</a>
							</dd>
						</div>
					{:else}
						<div class="flex justify-between gap-3 py-2.5">
							<dt class="text-text-muted">Camera catalog</dt>
							<dd class="font-mono text-text-faint">Unavailable</dd>
						</div>
					{/if}
				</dl>
				<div class="grid grid-cols-2 gap-2">
					<button
						type="button"
						class="h-8 rounded-sm border border-hairline bg-raised px-3 text-xs text-text-muted disabled:cursor-not-allowed"
						disabled>Update check unavailable</button
					>
					<button
						type="button"
						class="h-8 rounded-sm border border-hairline bg-raised px-3 text-xs text-text-muted disabled:cursor-not-allowed"
						disabled>Config backup unavailable</button
					>
				</div>
				<div class="border-t border-hairline pt-3">
					<p class="font-mono text-2xs tracking-caps text-live-text">RUNTIME COMMAND</p>
					<div class="mt-2 flex items-center justify-between gap-3">
						<div>
							<p class="text-sm font-medium">Restart the recorder</p>
							<p class="mt-1 text-xs text-text-faint">
								All camera recordings may have a brief gap.
							</p>
						</div>
						<button
							type="button"
							class="inline-flex h-8 shrink-0 items-center gap-2 rounded-sm border border-live/60 px-3 text-xs font-medium text-live-text disabled:opacity-45"
							disabled={restarting}
							onclick={onrestart}
						>
							<RotateCcwIcon class="size-3.5 {restarting ? 'animate-spin' : ''}" />
							{restarting ? 'Restarting' : 'Restart'}
						</button>
					</div>
				</div>
				<div class="flex items-center justify-between gap-3 border-t border-hairline pt-3">
					<div>
						<p class="text-sm font-medium">Erase all recordings</p>
						<p class="mt-1 text-xs text-text-faint">No destructive erase endpoint exists.</p>
					</div>
					<button
						type="button"
						class="h-8 rounded-sm border border-hairline px-3 text-xs text-text-faint disabled:cursor-not-allowed"
						disabled>Erase unavailable</button
					>
				</div>
			</article>

			<article class="space-y-4 p-5">
				<h3 class="flex items-center gap-2 text-base font-semibold">
					<FileTextIcon class="size-4" /> Logs & diagnostics
				</h3>
				<div class="rounded-sm border border-healthy/35 bg-healthy/5 px-3 py-3">
					<p class="text-sm font-medium">Live server and browser logs</p>
					<p class="mt-1 text-xs leading-5 text-text-muted">
						The dedicated viewer supports replay, streaming, filters, pause/resume, clear, and
						redacted JSONL export.
					</p>
				</div>
				<div class="grid grid-cols-2 gap-2">
					<a
						href={resolve('/settings/logs')}
						class="inline-flex h-8 items-center justify-center gap-2 rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary"
					>
						<FileTextIcon class="size-3.5" /> Open logs
					</a>
					<a
						href={resolve('/system-health')}
						class="inline-flex h-8 items-center justify-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
					>
						<ActivityIcon class="size-3.5" /> Open health
					</a>
				</div>
				<div class="rounded-sm border border-activity/45 bg-activity/5 px-3 py-3">
					<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-activity">
						<CircleAlertIcon class="size-3.5" /> FULL DIAGNOSTICS BUNDLE UNAVAILABLE
					</div>
					<p class="mt-1.5 text-xs leading-5 text-text-muted">
						The current export contains redacted server/browser logs and viewer metadata. It does
						not include sanitized config or the health document, so it is not labeled as Paper's
						bundle.
					</p>
				</div>
				<button
					type="button"
					class="inline-flex h-8 w-full items-center justify-center gap-2 rounded-sm border border-hairline bg-raised px-3 text-xs text-text-muted disabled:cursor-not-allowed"
					disabled
				>
					<DownloadIcon class="size-3.5" /> Diagnostics bundle unavailable
				</button>
				<div class="border-t border-hairline pt-3 text-xs leading-5 text-text-faint">
					<p>
						Executable: <span class="font-mono break-all"
							>{evidence.system.executable ?? 'Unavailable'}</span
						>
					</p>
					<p>
						Working directory: <span class="font-mono break-all"
							>{evidence.system.workingDirectory ?? 'Unavailable'}</span
						>
					</p>
				</div>
			</article>
		</div>
	</section>
{/if}
