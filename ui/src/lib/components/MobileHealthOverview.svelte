<script lang="ts">
	import { resolve } from '$app/paths';
	import { rankHealthFindings } from '$lib/health-presentation';
	import type { CameraHealth, HealthIssue, ServerHealthResponse } from '$lib/types';

	type Props = {
		health: ServerHealthResponse;
		paperFrame?: boolean;
	};

	let { health, paperFrame = false }: Props = $props();
	let findings = $derived(rankHealthFindings(health));
	let primary = $derived(findings[0] ?? null);
	let visibleFindings = $derived(findings.slice(0, 3));

	function diagnosisHref(camera: CameraHealth): string {
		return `${resolve('/system-health')}/camera/${encodeURIComponent(camera.id)}`;
	}

	function formatDuration(seconds: number): string {
		const days = Math.floor(seconds / 86_400);
		const hours = Math.floor((seconds % 86_400) / 3_600);
		const minutes = Math.floor((seconds % 3_600) / 60);
		if (days > 0) return `${days}d ${hours.toString().padStart(2, '0')}h`;
		if (hours > 0) return `${hours}h ${minutes}m`;
		return `${minutes}m`;
	}

	function findingDetail(issue: HealthIssue, camera: CameraHealth | null): string {
		if (!camera) return issue.scope;
		if (camera.detail) return camera.detail;
		const stream = camera.streams[0];
		if (stream?.reconnects !== undefined) return `${stream.reconnects} reconnect attempts`;
		if (camera.last_error) return camera.last_error;
		return `Current state: ${camera.state}`;
	}

	function primaryTitle(camera: CameraHealth | null, issue: HealthIssue): string {
		if (!camera) return issue.message;
		return /^[a-z]/.test(issue.message)
			? `${camera.name} ${issue.message}`
			: `${camera.name} · ${issue.message}`;
	}

	function findingTitle(issue: HealthIssue, camera: CameraHealth | null): string {
		if (!camera) return issue.message;
		return camera.state === 'degraded'
			? `${camera.name} ${issue.message}`
			: `${camera.name} ${camera.state}`;
	}

	function findingMetric(camera: CameraHealth | null): string {
		if (!camera) return '';
		const stream = camera.streams[0];
		if (stream?.report_age_ms !== undefined && stream.report_age_ms > 0) {
			return formatDuration(Math.round(stream.report_age_ms / 1_000));
		}
		if (stream?.fps !== undefined && stream.expected_fps) {
			return `${Math.max(0, Math.round((1 - stream.fps / stream.expected_fps) * 100))}%`;
		}
		return '';
	}

	function issueColor(issue: HealthIssue): string {
		return issue.severity === 'critical' ? 'bg-live' : 'bg-activity';
	}

	function count(value: number | undefined): string {
		return value === undefined ? '—' : `${value}`;
	}
</script>

<section
	data-mobile-health-overview
	class="flex w-full flex-col {paperFrame ? 'h-[702px]' : 'min-h-[calc(100svh-78px)]'} md:hidden"
	aria-label="Mobile health overview"
>
	<header class="flex h-[52px] shrink-0 items-center justify-between border-b border-hairline px-4">
		<h1 class="text-xl leading-6 font-bold">Health</h1>
		<span class="font-mono text-2xs leading-3 text-text-faint uppercase">
			Up {formatDuration(health.system.system_uptime_seconds)}
		</span>
	</header>

	<div class="flex h-[650px] shrink-0 flex-col gap-[14px] p-4">
		{#if primary}
			<section
				class="flex h-[196px] shrink-0 flex-col gap-2.5 rounded-md border border-live/40 bg-live/10 p-4"
				aria-label="Highest priority health issue"
				data-health-priority
			>
				<p
					class="flex h-3 items-center gap-2 font-mono text-2xs leading-3 tracking-[0.08em] text-live-text uppercase"
				>
					<span class="size-[7px] rounded-full bg-live"></span>
					{primary.issue.severity}
				</p>
				<h2 class="text-xl leading-6 font-semibold">
					{primaryTitle(primary.camera, primary.issue)}
				</h2>
				<p class="line-clamp-2 text-sm leading-[19.5px] text-text-muted">
					{primary.camera?.last_error ?? `Server-authored ${primary.issue.scope} evidence.`}
				</p>
				<div class="mt-auto flex h-8 items-center gap-2">
					{#if primary.camera}
						<a
							href={diagnosisHref(primary.camera)}
							class="ml-[84px] inline-flex h-8 items-center rounded-sm bg-primary px-3 text-xs-plus leading-4 font-semibold text-on-primary"
							aria-label={`Diagnose ${primary.camera.name}`}
						>
							Diagnose
						</a>
					{/if}
				</div>
			</section>
		{/if}

		<div class="flex h-5 shrink-0 items-center justify-between">
			<h2 class="text-lg leading-5 font-semibold">Open issues</h2>
			<span class="font-mono text-2xs leading-3 text-text-faint">{findings.length}</span>
		</div>

		<div class="h-[181px] shrink-0 overflow-hidden rounded-md border border-hairline bg-surface">
			{#each visibleFindings as finding, index (`${finding.issue.scope}-${finding.issue.message}-${index}`)}
				<div class="flex h-[60px] gap-2.5 border-b border-hairline p-[13px] last:border-b-0">
					<span class="mt-[5px] size-[7px] shrink-0 rounded-full {issueColor(finding.issue)}"
					></span>
					<div class="flex min-w-0 flex-1 flex-col gap-[3px]">
						<div class="flex items-center justify-between gap-3">
							<p class="truncate text-sm leading-4 font-semibold">
								{findingTitle(finding.issue, finding.camera)}
							</p>
							<span class="shrink-0 font-mono text-2xs leading-3 text-text-faint">
								{findingMetric(finding.camera)}
							</span>
						</div>
						<p class="truncate text-xs leading-[14px] text-text-muted">
							{findingDetail(finding.issue, finding.camera)}
						</p>
					</div>
				</div>
			{/each}
		</div>

		<div
			class="grid h-[59px] shrink-0 grid-cols-5 gap-px overflow-hidden rounded-sm border border-hairline bg-hairline"
			aria-label="Camera health dimensions"
		>
			{#each [['CONFIG', `${health.totals.configured_cameras}`], ['LINK', count(health.totals.connected_cameras)], ['FRESH', count(health.totals.fresh_cameras)], ['DECODE', count(health.totals.decodable_cameras)], ['RECORD', `${count(health.totals.recording_cameras)}/${count(health.totals.recording_requested_cameras)}`]] as dimension (dimension[0])}
				<div class="flex min-w-0 flex-col gap-[3px] bg-surface px-1 py-2.5 text-center">
					<p class="truncate font-mono text-2xs leading-3 text-text-faint">{dimension[0]}</p>
					<p class="truncate text-lg-plus leading-[22px]">{dimension[1]}</p>
				</div>
			{/each}
		</div>
	</div>
</section>
