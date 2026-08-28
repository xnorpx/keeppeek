<script lang="ts">
	import { resolve } from '$app/paths';
	import { integrationsEvidence } from '$lib/integrations';
	import DesktopPaperRail from './DesktopPaperRail.svelte';

	const evidence = integrationsEvidence();
	const homeAssistant = evidence.integrations.find(
		(integration) => integration.id === 'home-assistant'
	)!;
	const webhooks = evidence.integrations.find((integration) => integration.id === 'webhooks')!;
	const prometheus = evidence.integrations.find((integration) => integration.id === 'prometheus')!;
</script>

<section
	data-integrations-paper-frame
	class="flex h-[869px] w-[1440px] overflow-hidden rounded-lg border border-hairline bg-surface [font-synthesis:none]"
	aria-label="Integration evidence"
>
	<DesktopPaperRail />

	<div class="flex h-[867px] w-[1374px] shrink-0 flex-col">
		<header
			class="flex h-[52px] w-[1374px] shrink-0 items-center justify-between border-b border-hairline px-5"
		>
			<div class="flex items-baseline gap-3">
				<h2 class="text-base leading-5 font-semibold">Settings</h2>
				<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-muted">
					INTEGRATIONS
				</p>
			</div>
			<span class="flex items-center gap-2 text-[13px] leading-4 text-text-muted">
				<span class="size-1.5 rounded-full bg-healthy"></span>No third-party media relay
			</span>
		</header>

		<div class="flex h-[815px] w-[1374px] shrink-0 flex-col gap-5 px-8 py-7">
			<article
				data-integration-band="home-assistant"
				class="flex h-[205px] w-[1310px] shrink-0 overflow-hidden rounded-md border border-primary bg-surface"
			>
				<div
					class="flex h-[203px] w-[400px] shrink-0 flex-col gap-2.5 border-r border-hairline p-5"
				>
					<div class="flex h-[22px] shrink-0 items-center gap-2.5">
						<span class="size-2 rounded-full bg-text-faint"></span>
						<h3 class="text-lg leading-[22px] font-semibold">{homeAssistant.label}</h3>
					</div>
					<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-faint">
						DIRECT BROWSER · NO MEDIA PROXY
					</p>
					<p class="h-[63px] text-[13px] leading-[21px] text-text-muted">
						{homeAssistant.architecture}
						{homeAssistant.egress}
					</p>
					<button
						type="button"
						class="h-[30px] w-[180px] rounded-sm border border-hairline-strong text-[13px] text-text-faint"
						disabled>Card package unavailable</button
					>
				</div>

				<div class="flex h-[203px] w-[909px] shrink-0 flex-col p-5">
					<div class="flex h-[70px] shrink-0 gap-5 pb-4">
						<div class="flex w-[420px] shrink-0 flex-col gap-1.5">
							<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">
								ALLOWED ORIGIN
							</p>
							<div
								class="flex h-[34px] items-center rounded-sm border border-hairline-strong bg-raised px-2.5 font-mono text-[13px] text-text-faint"
							>
								Configuration unavailable
							</div>
						</div>
						<div class="flex w-[429px] shrink-0 flex-col gap-1.5">
							<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">
								DEDICATED TOKEN
							</p>
							<div
								class="flex h-[34px] items-center rounded-sm border border-hairline bg-raised px-2.5 font-mono text-[13px] text-text-faint"
							>
								Token registry unavailable
							</div>
						</div>
					</div>
					<div
						class="flex h-[72px] shrink-0 items-start gap-3 rounded-sm border border-activity/35 bg-activity/5 p-3.5"
					>
						<span class="mt-1 size-2 shrink-0 rounded-full bg-activity"></span>
						<p class="w-[820px] text-[13px] leading-[21px] text-text-muted">
							Exact-origin CORS and Bearer access exist, but no approved card package, per-key scope
							registry, origin editor, or token rotation command is available here.
						</p>
					</div>
				</div>
			</article>

			<article
				data-integration-band="mqtt"
				class="flex h-[236px] w-[1310px] shrink-0 overflow-hidden rounded-md border border-hairline bg-surface"
			>
				<div
					class="flex h-[234px] w-[400px] shrink-0 flex-col gap-2.5 border-r border-hairline p-5"
				>
					<div class="flex h-[22px] shrink-0 items-center gap-2.5">
						<span class="size-2 rounded-full bg-healthy"></span>
						<h3 class="text-lg leading-[22px] font-semibold">MQTT 5</h3>
					</div>
					<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-faint">
						CONNECTED · MQTT 5 · QOS 1
					</p>
					<p class="h-[63px] text-[13px] leading-[21px] text-text-muted">
						Committed event revisions and camera health transitions, delivered durably. Retained
						status lets late subscribers see current forwarder health.
					</p>
					<button
						type="button"
						class="h-[30px] w-[229px] rounded-sm border border-hairline bg-raised text-[13px] text-text-faint"
						>Edit broker · Applies live</button
					>
				</div>

				<div class="flex h-[234px] w-[909px] shrink-0 flex-col gap-4 p-5">
					<div class="flex h-[54px] shrink-0 gap-4">
						<div class="flex w-[340px] shrink-0 flex-col gap-1.5">
							<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">
								BROKER
							</p>
							<div
								class="flex h-[34px] items-center rounded-sm border border-hairline-strong bg-raised px-2.5 font-mono text-[13px] text-foreground"
							>
								mqtts://broker.home:8883
							</div>
						</div>
						<div class="flex w-[260px] shrink-0 flex-col gap-1.5">
							<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">
								TOPIC PREFIX
							</p>
							<div
								class="flex h-[34px] items-center rounded-sm border border-hairline-strong bg-raised px-2.5 font-mono text-[13px] text-foreground"
							>
								keeppeek
							</div>
						</div>
						<div class="flex w-[253px] shrink-0 flex-col gap-1.5">
							<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">
								MQTT 5 QOS
							</p>
							<div
								class="flex h-[34px] overflow-hidden rounded-sm border border-hairline-strong text-[13px]"
							>
								<span class="grid w-[84px] place-items-center text-text-muted">0</span>
								<span
									class="grid w-[84px] place-items-center bg-primary font-semibold text-on-primary"
									>1</span
								>
								<span class="grid w-[83px] place-items-center text-text-muted">2</span>
							</div>
						</div>
					</div>
					<div class="flex h-[124px] shrink-0 flex-col gap-1.5">
						<p class="font-mono text-2xs leading-[14px] tracking-[0.12em] text-text-faint">
							TOPICS PUBLISHED
						</p>
						<div
							class="flex h-[104px] flex-col justify-center rounded-sm bg-raised px-3.5 font-mono text-xs leading-5 text-text-faint"
						>
							<span>keeppeek/home-nvr/sources/&lt;source&gt;/events/&lt;type&gt;</span>
							<span>&nbsp;&nbsp;JSON revision · correlation data = stable event ID</span>
							<span>keeppeek/home-nvr/forwarders/mqtt/status</span>
							<span>&nbsp;&nbsp;retained health · QoS 1 · MQTT 5 only</span>
						</div>
					</div>
				</div>
			</article>

			<div
				data-integration-band="pair"
				class="flex h-[278px] w-[1310px] shrink-0 items-start gap-5"
			>
				<article
					class="flex h-[270px] w-[645px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-5"
				>
					<div class="flex h-[22px] shrink-0 items-center gap-2.5">
						<span class="size-2 rounded-full bg-text-faint"></span>
						<h3 class="text-lg leading-[22px] font-semibold">{webhooks.label}</h3>
						<span class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-faint"
							>ENDPOINTS UNAVAILABLE</span
						>
					</div>
					<p class="h-[42px] text-[13px] leading-[21px] text-text-muted">
						{webhooks.architecture}
						{webhooks.egress}
					</p>
					<div class="flex h-[88px] shrink-0 flex-col">
						{#each ['Endpoint registry unavailable', 'Signing and retry state unavailable'] as row (row)}
							<div
								class="flex h-11 shrink-0 items-center justify-between border-y border-hairline font-mono text-[13px] text-text-faint"
							>
								<span>{row}</span><span>—</span>
							</div>
						{/each}
					</div>
					<button
						type="button"
						class="h-[34px] w-[232px] rounded-sm border border-hairline bg-raised text-[13px] text-text-faint"
						disabled
					>
						Add webhook unavailable
					</button>
				</article>

				<article
					class="flex h-[278px] w-[645px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-5"
				>
					<div class="flex h-[22px] shrink-0 items-center gap-2.5">
						<span class="size-2 rounded-full bg-healthy"></span>
						<h3 class="text-lg leading-[22px] font-semibold">{prometheus.label}</h3>
						<span class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-faint"
							>ENDPOINT AVAILABLE</span
						>
					</div>
					<p class="h-[42px] text-[13px] leading-[21px] text-text-muted">
						{prometheus.architecture}
						{prometheus.egress}
					</p>
					<div class="flex h-36 shrink-0 flex-col">
						<div class="flex h-9 shrink-0 items-center justify-between border-t border-hairline">
							<span class="font-mono text-xs">{prometheus.implementedEndpoint}</span>
							<a href="/metrics" class="text-xs text-primary-soft">Open metrics</a>
						</div>
						{#each ['Collector configuration', 'Last scrape evidence', 'External alert state'] as row (row)}
							<div
								class="flex h-9 shrink-0 items-center justify-between border-t border-hairline text-xs text-text-muted"
							>
								<span>{row}</span><span class="font-mono text-text-faint">Unavailable</span>
							</div>
						{/each}
					</div>
				</article>
			</div>
		</div>
	</div>

	<div class="sr-only">
		<a href={resolve('/system-health')}>Health</a>
		<a href={resolve('/settings/logs')}>Logs</a>
	</div>
</section>
