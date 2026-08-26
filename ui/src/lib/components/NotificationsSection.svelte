<script lang="ts">
	import { resolve } from '$app/paths';
	import { capabilityActions } from '$lib/capability-actions';
	import { useCapabilityState } from '$lib/capability-context';
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import { notificationsEvidence } from '$lib/notifications';
	import BellIcon from '@lucide/svelte/icons/bell';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import MailIcon from '@lucide/svelte/icons/mail';
	import MonitorIcon from '@lucide/svelte/icons/monitor';
	import RadioTowerIcon from '@lucide/svelte/icons/radio-tower';
	import NotificationsPaperFrame from './NotificationsPaperFrame.svelte';
	import NotificationsRuntime from './NotificationsRuntime.svelte';

	type Props = {
		paperFrame?: boolean;
	};

	let { paperFrame = false }: Props = $props();

	const evidence = notificationsEvidence();
	const capabilities = useCapabilityState();
	let runtimeSupported = $derived(capabilities.supports('keeppeek.rules.v1'));
	const icons = {
		push: BellIcon,
		email: MailIcon,
		browser: MonitorIcon,
		integrations: RadioTowerIcon
	} as const;
	const fieldLabels = {
		'event-or-health-condition': 'Event or health condition',
		'camera-or-group-scope': 'Camera or group scope',
		destinations: 'Human-facing destinations',
		cooldown: 'Cooldown and rate limit',
		'quiet-hours-policy': 'Quiet-hours policy and critical bypass'
	} as const;
</script>

{#if paperFrame}
	<NotificationsPaperFrame />
{:else if runtimeSupported}
	<NotificationsRuntime />
{:else}
	<section
		id="notifications"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
		aria-labelledby="notifications-heading"
	>
		<header
			class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">NOTIFICATIONS · TARGET</p>
				<h2 id="notifications-heading" class="mt-1 text-xl font-semibold">
					A notification you ignore is a bug
				</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">
					Paper defines human-facing channels, factual firing counts, cooldowns, quiet hours, tests,
					retries, and delivery receipts. None has a runtime endpoint in this server build.
				</p>
			</div>
			<div class="flex flex-wrap gap-2">
				<CapabilityGate {...capabilityActions.addRule} />
				<CapabilityGate {...capabilityActions.sendNotificationTest} />
			</div>
		</header>

		<div class="grid border-b border-hairline lg:grid-cols-4">
			{#each evidence.channels as channel, index (channel.id)}
				{@const Icon = icons[channel.id]}
				<article
					class="space-y-3 border-b border-hairline p-4 lg:border-b-0 {index < 3
						? 'lg:border-r'
						: ''}"
				>
					<div class="flex items-center gap-2">
						<span
							class="grid size-8 place-items-center rounded-sm border border-hairline bg-raised text-primary-soft"
						>
							<Icon class="size-4" />
						</span>
						<div>
							<h3 class="text-sm font-semibold">{channel.label}</h3>
							<p class="font-mono text-2xs tracking-caps text-text-faint">UNAVAILABLE</p>
						</div>
					</div>
					<p class="text-xs leading-5 text-text-muted">{channel.intendedBehavior}</p>
					<dl class="space-y-1 border-t border-hairline pt-2 font-mono text-2xs text-text-faint">
						<div class="flex justify-between gap-2">
							<dt>Configuration</dt>
							<dd>Unavailable</dd>
						</div>
						<div class="flex justify-between gap-2">
							<dt>Health</dt>
							<dd>Unavailable</dd>
						</div>
						<div class="flex justify-between gap-2">
							<dt>Last delivery</dt>
							<dd>Unavailable</dd>
						</div>
					</dl>
				</article>
			{/each}
		</div>

		<div class="grid border-b border-hairline lg:grid-cols-[1.15fr_0.85fr]">
			<div class="space-y-3 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div class="flex flex-wrap items-baseline justify-between gap-2">
					<h3 class="text-base font-semibold">Target rule anatomy</h3>
					<span class="font-mono text-2xs tracking-caps text-activity">NOT ENFORCED</span>
				</div>
				<ol class="divide-y divide-hairline border-y border-hairline">
					{#each evidence.ruleFields as field, index (field)}
						<li class="flex min-h-10 items-center gap-3 py-2 text-xs">
							<span
								class="grid size-5 shrink-0 place-items-center rounded-full border border-hairline-strong bg-raised font-mono text-2xs"
								>{index + 1}</span
							>
							<span>{fieldLabels[field]}</span>
						</li>
					{/each}
				</ol>
				<p class="text-xs leading-5 text-text-faint">
					A future firing count belongs to rule execution history. It cannot be derived from event
					catalog totals, camera motion counters, or current health issues.
				</p>
			</div>

			<div class="space-y-4 p-5">
				<div class="rounded-sm border border-activity/45 bg-activity/5 px-3 py-3" role="status">
					<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-activity">
						<CircleAlertIcon class="size-3.5" /> RULES RUNTIME UNAVAILABLE
					</div>
					<p class="mt-1.5 text-xs leading-5 text-text-muted">
						No list, create, edit, enable, disable, test, retry, quiet-hours, delivery-history, or
						browser-permission contract is implemented.
					</p>
				</div>
				<dl class="divide-y divide-hairline border-y border-hairline text-xs">
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Configured rules</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Fired in last 7 days</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Quiet hours</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Retry queue</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
				</dl>
				<CapabilityGate
					{...capabilityActions.manageNotificationChannels}
					class="w-full justify-start"
				/>
			</div>
		</div>

		<footer
			class="flex flex-col gap-3 bg-raised/40 px-5 py-4 sm:flex-row sm:items-center sm:justify-between"
		>
			<div>
				<p class="font-mono text-2xs tracking-caps text-text-faint">MACHINE DELIVERY IS SEPARATE</p>
				<p class="mt-1 text-xs text-text-muted">
					MQTT and webhook architectures belong to Integrations; a human notification rule must not
					be presented as proof those runtimes exist.
				</p>
			</div>
			<a
				href={`${resolve('/settings')}#integrations`}
				class="inline-flex h-8 shrink-0 items-center rounded-sm border border-hairline-strong bg-surface px-3 text-xs font-medium"
			>
				Open integration contracts
			</a>
		</footer>
	</section>
{/if}
