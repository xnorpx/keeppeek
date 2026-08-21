<script lang="ts">
	import { resolve } from '$app/paths';
	import { useAppearanceState } from '$lib/appearance-context';
	import { appearanceSystemEvidence, type ThemePreference } from '$lib/appearance-system';
	import type { ServerHealthResponse } from '$lib/types';

	type Props = {
		health: ServerHealthResponse | null;
		browserTimeZone: string | null;
		prefersReducedMotion: boolean | null;
		restarting?: boolean;
		logLines?: readonly string[];
		logsLive?: boolean;
		logTarget?: string;
		onrestart: () => void;
	};

	let {
		health,
		browserTimeZone,
		prefersReducedMotion,
		restarting = false,
		logLines = [],
		logsLive = false,
		logTarget = 'all',
		onrestart
	}: Props = $props();
	const appearance = useAppearanceState();
	let evidence = $derived(appearanceSystemEvidence(health));
	let selectedLogLevel = $state<'all' | 'error' | 'warn'>('all');
	let visibleLogLines = $derived(
		selectedLogLevel === 'all'
			? logLines
			: logLines.filter((line) => line.includes(selectedLogLevel.toUpperCase()))
	);

	const themeOptions: Array<{ value: ThemePreference; label: string }> = [
		{ value: 'dark', label: 'Dark' },
		{ value: 'light', label: 'Light' },
		{ value: 'system', label: 'Match system' }
	];

	function formatDuration(seconds: number | null): string {
		if (seconds === null) return 'Unavailable';
		const days = Math.floor(seconds / 86_400);
		const hours = Math.floor((seconds % 86_400) / 3_600);
		const minutes = Math.floor((seconds % 3_600) / 60);
		if (days > 0) {
			return `${days}d ${hours.toString().padStart(2, '0')}h ${minutes.toString().padStart(2, '0')}m`;
		}
		if (hours > 0) return `${hours}h ${minutes}m`;
		return `${minutes}m`;
	}
</script>

<section
	data-appearance-system-paper-frame
	class="flex h-[581px] w-[1440px] items-start gap-5 overflow-hidden bg-ground [font-synthesis:none]"
	aria-label="Appearance, system, and logs"
>
	<article
		data-settings-panel="appearance"
		class="flex h-[548px] w-[466px] shrink-0 flex-col gap-4 rounded-md border border-hairline bg-surface p-[22px]"
	>
		<h2 class="h-6 text-xl leading-6 font-semibold">Appearance & time</h2>

		<div class="flex h-[98px] shrink-0 flex-col gap-1.5">
			<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">TIME ZONE</p>
			<div
				class="flex h-9 shrink-0 items-center justify-between rounded-sm border border-activity/55 bg-raised px-3"
			>
				<span class="text-sm leading-[18px]">{browserTimeZone ?? 'Unavailable'}</span>
				<span class="font-mono text-2xs leading-[14px] text-text-faint">BROWSER ONLY</span>
			</div>
			<p class="h-9 text-xs-plus leading-[18px] text-text-faint">
				Server timezone and update commands are unavailable; history is not relabeled by this
				browser value.
			</p>
		</div>

		<div class="flex h-[54px] shrink-0 flex-col gap-1.5">
			<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">CLOCK</p>
			<div
				class="flex h-[34px] shrink-0 overflow-hidden rounded-sm border border-hairline-strong"
				role="group"
				aria-label="Clock preference unavailable"
			>
				<button type="button" class="w-[210px] text-[13px] leading-4 text-text-muted" disabled>
					24 hour
				</button>
				<button type="button" class="w-[210px] text-[13px] leading-4 text-text-muted" disabled>
					12 hour
				</button>
			</div>
		</div>

		<div class="flex h-[54px] shrink-0 flex-col gap-1.5">
			<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">WEEK STARTS</p>
			<div
				class="flex h-[34px] shrink-0 overflow-hidden rounded-sm border border-hairline-strong"
				role="group"
				aria-label="Week-start preference unavailable"
			>
				{#each ['Monday', 'Sunday', 'Saturday'] as day (day)}
					<button type="button" class="w-[140px] text-[13px] leading-4 text-text-muted" disabled>
						{day}
					</button>
				{/each}
			</div>
		</div>

		<div class="flex h-24 shrink-0 flex-col gap-1.5">
			<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">THEME</p>
			<div
				class="flex h-[34px] shrink-0 overflow-hidden rounded-sm border border-hairline-strong"
				role="group"
				aria-label="Theme preference"
			>
				{#each themeOptions as option (option.value)}
					<button
						type="button"
						class="w-[140px] text-[13px] leading-4 {appearance.preference === option.value
							? 'font-semibold text-foreground'
							: 'text-text-muted'}"
						aria-pressed={appearance.preference === option.value}
						onclick={() => appearance.setPreference(option.value)}
					>
						{option.label}
					</button>
				{/each}
			</div>
			<p class="h-9 text-xs-plus leading-[18px] text-text-faint">
				Dark is the default for video. Light uses the same semantic roles, while video stays dark in
				both.
			</p>
		</div>

		<div class="flex h-10 shrink-0 items-center justify-between border-t border-hairline">
			<span class="text-sm leading-[18px]">Language</span>
			<span class="flex items-center gap-2.5">
				<span class="text-[13px] leading-4 text-text-muted">{evidence.language}</span>
				<span class="font-mono text-[10px] leading-3 tracking-[0.08em] text-text-faint">
					ONLY ONE TODAY
				</span>
			</span>
		</div>

		<div class="flex h-10 shrink-0 items-center justify-between border-t border-hairline">
			<span class="text-sm leading-[18px]">Reduce motion</span>
			<span
				class="flex h-[19px] w-[34px] shrink-0 items-center rounded-full bg-hairline-strong p-0.5 {prefersReducedMotion
					? 'justify-end'
					: 'justify-start'}"
				role="status"
				aria-label={prefersReducedMotion === null
					? 'Reduced-motion preference unavailable'
					: prefersReducedMotion
						? 'Reduced motion requested by browser'
						: 'No reduced-motion browser preference'}
			>
				<span class="size-[15px] rounded-full bg-text-faint"></span>
			</span>
		</div>
	</article>

	<article
		data-settings-panel="system"
		class="flex h-[581px] w-[466px] shrink-0 flex-col gap-4 rounded-md border border-hairline bg-surface p-[22px]"
	>
		<h2 class="h-6 text-xl leading-6 font-semibold">System & updates</h2>

		<div
			class="flex h-[90px] shrink-0 flex-col gap-1.5 rounded-sm border border-activity/35 bg-activity/5 p-3.5"
		>
			<p class="text-sm leading-[18px] font-semibold">This is a proof of concept</p>
			<p class="text-xs-plus leading-[18px] text-text-muted">
				Not production ready. The version string says so and no screen in the product implies
				otherwise.
			</p>
		</div>

		<dl class="flex h-[200px] shrink-0 flex-col">
			<div class="flex h-10 shrink-0 items-center justify-between border-b border-hairline">
				<dt class="text-sm text-text-muted">Version</dt>
				<dd class="font-mono text-[13px] leading-4">{evidence.system.version ?? 'Unavailable'}</dd>
			</div>
			<div class="flex h-10 shrink-0 items-center justify-between border-b border-hairline">
				<dt class="text-sm text-text-muted">Running as</dt>
				<dd class="font-mono text-[13px] leading-4">
					{evidence.system.processName ?? 'Process unavailable'} · {evidence.system
						.operatingSystem ?? 'OS unavailable'}
				</dd>
			</div>
			<div class="flex h-10 shrink-0 items-center justify-between border-b border-hairline">
				<dt class="text-sm text-text-muted">Uptime</dt>
				<dd class="font-mono text-[13px] leading-4">
					{formatDuration(evidence.system.uptimeSeconds)}
				</dd>
			</div>
			<div class="flex h-10 shrink-0 items-center justify-between border-b border-hairline">
				<dt class="text-sm text-text-muted">Update channel</dt>
				<dd class="font-mono text-[13px] leading-4 text-text-faint">Unavailable</dd>
			</div>
			<div class="flex h-10 shrink-0 items-center justify-between border-b border-hairline">
				<dt class="text-sm text-text-muted">Config file</dt>
				<dd class="font-mono text-xs-plus leading-4 text-text-faint">Path unavailable</dd>
			</div>
		</dl>

		<div class="flex h-[34px] shrink-0 items-center gap-2.5">
			<button
				type="button"
				class="h-[34px] rounded-sm border border-hairline-strong px-3.5 text-[13px] text-text-muted"
				disabled>Check for updates</button
			>
			<button
				type="button"
				class="h-[34px] rounded-sm border border-hairline-strong px-3.5 text-[13px] text-text-muted"
				disabled>Back up config</button
			>
		</div>

		<div class="flex h-[123px] shrink-0 flex-col gap-2.5 border-t border-hairline pt-1.5">
			<p class="pt-2.5 font-mono text-2xs leading-[14px] tracking-[0.12em] text-live-text">
				DESTRUCTIVE
			</p>
			<div class="flex h-9 shrink-0 items-center justify-between">
				<div class="flex w-[280px] shrink-0 flex-col gap-0.5">
					<p class="text-sm leading-[18px]">Restart the recorder</p>
					<p class="text-xs-plus leading-4 text-text-faint">
						A gap of a few seconds on every camera.
					</p>
				</div>
				<button
					type="button"
					class="h-[30px] rounded-sm border border-live px-3 text-[13px] leading-4 text-live-text disabled:opacity-45"
					disabled={restarting}
					onclick={onrestart}
				>
					{restarting ? 'Restarting' : 'Restart'}
				</button>
			</div>
			<div class="flex h-9 shrink-0 items-center justify-between">
				<div class="flex w-[280px] shrink-0 flex-col gap-0.5">
					<p class="text-sm leading-[18px]">Erase all recordings</p>
					<p class="text-xs-plus leading-4 text-text-faint">No destructive erase command exists.</p>
				</div>
				<button
					type="button"
					class="h-[30px] rounded-sm border border-live px-3 text-[13px] leading-4 text-live-text"
					disabled
				>
					Erase
				</button>
			</div>
		</div>
	</article>

	<article
		data-settings-panel="logs"
		class="flex h-[391px] w-[468px] shrink-0 flex-col gap-4 rounded-md border border-hairline bg-surface p-[22px]"
	>
		<div class="flex h-6 shrink-0 items-baseline justify-between">
			<h2 class="text-xl leading-6 font-semibold">Logs & diagnostics</h2>
			<span class="flex items-center gap-[7px]">
				<span class="size-1.5 rounded-full {logsLive ? 'bg-white/85' : 'bg-text-faint'}"></span>
				<span class="font-mono text-2xs leading-[14px] text-text-muted">
					{logsLive ? 'LIVE' : 'VIEWER'}
				</span>
			</span>
		</div>

		<div class="flex h-7 shrink-0 gap-2" aria-label="Log filters">
			{#each [{ value: 'all', label: 'All' }, { value: 'warn', label: 'Warn' }, { value: 'error', label: 'Error' }] as filter (filter.value)}
				<button
					type="button"
					class="h-7 rounded-full px-2.5 text-xs-plus {selectedLogLevel === filter.value
						? 'bg-primary font-semibold text-on-primary'
						: 'border border-hairline-strong text-text-muted'}"
					onclick={() => (selectedLogLevel = filter.value as typeof selectedLogLevel)}
				>
					{filter.label}
				</button>
			{/each}
			<span
				class="inline-flex h-7 w-[99px] items-center justify-center rounded-full border border-hairline-strong px-2.5 text-xs-plus text-text-muted"
			>
				Target: {logTarget}
			</span>
		</div>

		<div
			class="h-[159px] shrink-0 overflow-hidden rounded-sm border border-hairline bg-ground p-3"
			aria-label="Deterministic log preview"
		>
			{#if visibleLogLines.length > 0}
				<pre
					class="font-mono text-[11px] leading-[19px] whitespace-pre text-text-muted">{visibleLogLines.join(
						'\n'
					)}</pre>
			{:else}
				<p class="text-xs leading-5 text-text-muted">
					No embedded log evidence. Open the live viewer for server and browser logs.
				</p>
			{/if}
		</div>

		<div class="flex h-[34px] shrink-0 items-center gap-2.5">
			<a
				href={resolve('/settings/logs')}
				class="inline-flex h-[34px] w-[162px] items-center justify-center rounded-sm bg-primary px-3.5 text-[13px] font-semibold text-on-primary"
			>
				Open logs
			</a>
			<a
				href={resolve('/system-health')}
				class="inline-flex h-[34px] w-[139px] items-center justify-center rounded-sm border border-hairline-strong px-3.5 text-[13px]"
			>
				Open health
			</a>
		</div>

		<p class="h-9 shrink-0 text-xs-plus leading-[18px] text-text-faint">
			Redacted log export is available in the viewer. A bundle with sanitized config and health
			evidence is not exposed.
		</p>
	</article>
</section>
