<script lang="ts">
	import { resolve } from '$app/paths';
	import { integrationsEvidence } from '$lib/integrations';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import HomeIcon from '@lucide/svelte/icons/house';
	import RadioTowerIcon from '@lucide/svelte/icons/radio-tower';
	import WebhookIcon from '@lucide/svelte/icons/webhook';
	import IntegrationsPaperFrame from './IntegrationsPaperFrame.svelte';
	import MqttIntegrationPanel from './MqttIntegrationPanel.svelte';

	type Props = {
		paperFrame?: boolean;
	};

	let { paperFrame = false }: Props = $props();

	const evidence = integrationsEvidence();
	const metricsEndpoint = '/metrics';
	const icons = {
		'home-assistant': HomeIcon,
		'mqtt-forwarder': RadioTowerIcon,
		webhooks: WebhookIcon,
		prometheus: ActivityIcon
	} as const;
</script>

{#if paperFrame}
	<IntegrationsPaperFrame />
{:else}
	<section
		id="integrations"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
		aria-labelledby="integrations-heading"
	>
		<header
			class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">INTEGRATIONS</p>
				<h2 id="integrations-heading" class="mt-1 text-xl font-semibold">
					Everything has an explicit egress boundary
				</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">
					MQTT forwarding has a durable runtime and explicit broker health. Other integration states
					remain unavailable until their own contracts are implemented.
				</p>
			</div>
			<span
				class="inline-flex h-8 items-center gap-2 rounded-full border border-hairline px-3 font-mono text-2xs tracking-caps text-text-muted"
			>
				<span class="size-1.5 rounded-full bg-healthy"></span> NO THIRD-PARTY MEDIA RELAY
			</span>
		</header>

		<div class="divide-y divide-hairline">
			{#each evidence.integrations as integration (integration.id)}
				{#if integration.id === 'mqtt-forwarder'}
					<MqttIntegrationPanel />
				{:else}
					{@const Icon = icons[integration.id]}
					<article class="grid gap-4 p-5 lg:grid-cols-[17rem_minmax(0,1fr)_17rem] lg:items-start">
						<div>
							<div class="flex items-center gap-2">
								<span
									class="grid size-8 place-items-center rounded-sm border border-hairline bg-raised text-primary-soft"
								>
									<Icon class="size-4" />
								</span>
								<div>
									<h3 class="text-base font-semibold">{integration.label}</h3>
									<p class="font-mono text-2xs tracking-caps text-text-faint">
										RUNTIME UNAVAILABLE
									</p>
								</div>
							</div>
							<button
								type="button"
								class="mt-3 h-8 rounded-sm border border-hairline bg-raised px-3 text-xs text-text-muted disabled:cursor-not-allowed"
								disabled
							>
								Configuration unavailable
							</button>
						</div>

						<div class="space-y-3">
							<div>
								<p class="font-mono text-2xs tracking-caps text-text-faint">ARCHITECTURE</p>
								<p class="mt-1 text-sm text-foreground">{integration.architecture}</p>
							</div>
							<div>
								<p class="font-mono text-2xs tracking-caps text-text-faint">WHAT WOULD LEAVE</p>
								<p class="mt-1 text-xs leading-5 text-text-muted">{integration.egress}</p>
							</div>
							{#if integration.implementedEndpoint}
								<div>
									<p class="font-mono text-2xs tracking-caps text-text-faint">
										IMPLEMENTED ENDPOINT
									</p>
									<p class="mt-1 font-mono text-xs text-foreground">
										{integration.implementedEndpoint}
									</p>
								</div>
							{/if}
						</div>

						<div class="rounded-sm border border-activity/45 bg-activity/5 px-3 py-3">
							<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-activity">
								<CircleAlertIcon class="size-3.5" /> MISSING CONTRACTS
							</div>
							<ul class="mt-2 space-y-1 text-xs text-text-muted">
								{#each integration.prerequisites as prerequisite (prerequisite)}
									<li class="flex gap-2">
										<span aria-hidden="true">·</span><span>{prerequisite}</span>
									</li>
								{/each}
							</ul>
						</div>
					</article>
				{/if}
			{/each}
		</div>

		<footer
			class="grid gap-4 border-t border-hairline bg-raised/40 px-5 py-4 lg:grid-cols-[minmax(0,1fr)_auto] lg:items-center"
		>
			<div>
				<p class="font-mono text-2xs tracking-caps text-text-faint">
					AVAILABLE OPERATIONAL EVIDENCE
				</p>
				<p class="mt-1 text-xs leading-5 text-text-muted">
					Server health, metrics, and redacted logs are implemented KeepPeek surfaces. They do not
					prove that an external collector, broker, card, or webhook is configured.
				</p>
			</div>
			<div class="flex flex-wrap gap-2">
				<a
					href={resolve('/system-health')}
					class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-surface px-3 text-xs font-medium"
				>
					<ActivityIcon class="size-3.5" /> Health
				</a>
				<a
					href={resolve('/settings/logs')}
					class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-surface px-3 text-xs font-medium"
				>
					<ExternalLinkIcon class="size-3.5" /> Logs
				</a>
				<a
					href={metricsEndpoint}
					class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-surface px-3 text-xs font-medium"
				>
					<ActivityIcon class="size-3.5" /> Metrics
				</a>
			</div>
		</footer>
	</section>
{/if}
