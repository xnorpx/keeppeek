<script lang="ts">
	import { capabilityActions } from '$lib/capability-actions';
	import { notificationsEvidence } from '$lib/notifications';
	import CapabilityGate from './CapabilityGate.svelte';
	import DesktopPaperRail from './DesktopPaperRail.svelte';

	const evidence = notificationsEvidence();
	const ruleRows = [
		{
			when: 'Event or health condition',
			meta: 'NOT ENFORCED',
			where: 'Scope unavailable',
			destination: 'Unavailable',
			cooldown: '—'
		},
		{
			when: 'Camera or group scope',
			meta: 'NO RULE REGISTRY',
			where: 'Unavailable',
			destination: 'Unavailable',
			cooldown: '—'
		},
		{
			when: 'Destinations and cooldown',
			meta: 'NO DELIVERY RUNTIME',
			where: 'Unavailable',
			destination: 'Unavailable',
			cooldown: '—'
		},
		{
			when: 'Quiet hours and critical bypass',
			meta: 'NO POLICY RUNTIME',
			where: 'Unavailable',
			destination: 'Unavailable',
			cooldown: '—'
		}
	] as const;
</script>

<section
	data-notifications-paper-frame
	class="flex h-[1075px] w-[1440px] overflow-hidden rounded-lg border border-hairline bg-surface [font-synthesis:none]"
	aria-label="Notification evidence"
>
	<DesktopPaperRail />

	<div class="flex h-[1073px] w-[1374px] shrink-0 flex-col">
		<header
			class="flex h-[52px] w-[1374px] shrink-0 items-center justify-between border-b border-hairline px-5"
		>
			<div class="flex items-baseline gap-3">
				<h2 class="text-base leading-5 font-semibold">Settings</h2>
				<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-muted">
					NOTIFICATIONS · TARGET · rules.v1
				</p>
			</div>
			<div class="flex items-center gap-2.5">
				<CapabilityGate
					{...capabilityActions.addRule}
					class="h-[30px] min-h-[30px] px-3 text-[11px]"
				/>
				<CapabilityGate
					{...capabilityActions.sendNotificationTest}
					class="h-[30px] min-h-[30px] px-3 text-[11px]"
				/>
			</div>
		</header>

		<div class="flex h-[1021px] w-[1374px] shrink-0 flex-col gap-[26px] px-8 py-7">
			<section
				data-notification-band="channels"
				class="flex h-[195px] w-[1310px] shrink-0 flex-col gap-3"
				aria-labelledby="notification-channels-heading"
			>
				<div class="flex h-[22px] shrink-0 items-baseline justify-between">
					<h3 id="notification-channels-heading" class="text-lg leading-[22px] font-semibold">
						Where they go
					</h3>
					<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-faint">
						CHANNEL CONFIGURATION, HEALTH, AND DELIVERY HISTORY UNAVAILABLE
					</p>
				</div>
				<div class="flex h-[161px] shrink-0 gap-4">
					{#each evidence.channels as channel (channel.id)}
						<article
							data-notification-channel={channel.id}
							class="flex h-[161px] w-[311px] shrink-0 flex-col gap-2.5 rounded-md border border-hairline bg-surface p-4"
						>
							<div class="flex h-[19px] shrink-0 items-center justify-between">
								<h4 class="text-[15px] leading-[18px] font-semibold">{channel.label}</h4>
								<span
									class="flex h-[19px] w-[34px] items-center rounded-full bg-hairline-strong p-0.5"
								>
									<span class="size-[15px] rounded-full bg-text-faint"></span>
								</span>
							</div>
							<p class="font-mono text-2xs leading-[14px] text-text-faint">UNAVAILABLE</p>
							<p class="h-9 text-xs leading-[18px] text-text-muted">
								{channel.intendedBehavior}
							</p>
							<p class="pt-0.5 font-mono text-2xs leading-4 text-text-faint">
								CONFIG · HEALTH · DELIVERY NOT RETURNED
							</p>
						</article>
					{/each}
				</div>
			</section>

			<section
				data-notification-band="rules"
				class="flex h-72 w-[1310px] shrink-0 flex-col"
				aria-labelledby="notification-rules-heading"
			>
				<div class="flex h-[34px] shrink-0 items-baseline justify-between pb-3">
					<h3 id="notification-rules-heading" class="text-lg leading-[22px] font-semibold">
						Rules
					</h3>
					<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-faint">
						FIRING HISTORY UNAVAILABLE
					</p>
				</div>
				<div
					class="flex h-[30px] shrink-0 items-center border-b border-hairline-strong font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-faint"
				>
					<span class="w-[360px] shrink-0">WHEN</span>
					<span class="w-[210px] shrink-0">WHERE</span>
					<span class="w-[200px] shrink-0">SEND TO</span>
					<span class="w-[120px] shrink-0">COOLDOWN</span>
					<span class="w-[260px] shrink-0">FIRED LAST 7 DAYS</span>
					<span class="w-40 shrink-0"></span>
				</div>
				{#each ruleRows as row (row.when)}
					<div
						data-notification-rule-evidence
						class="flex h-14 w-[1310px] shrink-0 items-center border-b border-hairline"
					>
						<div class="flex w-[360px] shrink-0 flex-col gap-[3px] pr-4">
							<span class="text-sm leading-[18px] font-medium">{row.when}</span>
							<span class="font-mono text-2xs leading-[14px] text-text-faint">{row.meta}</span>
						</div>
						<span class="w-[210px] shrink-0 pr-4 text-[13px] leading-4 text-text-muted">
							{row.where}
						</span>
						<span class="w-[200px] shrink-0 pr-4 text-[13px] leading-4 text-text-muted">
							{row.destination}
						</span>
						<span class="w-[120px] shrink-0 font-mono text-[13px] leading-4 text-text-faint">
							{row.cooldown}
						</span>
						<span class="w-[260px] shrink-0 font-mono text-[13px] leading-4 text-text-faint">
							Unavailable
						</span>
						<span class="flex w-40 shrink-0 justify-end">
							<span
								class="flex h-[19px] w-[34px] justify-end rounded-full bg-hairline-strong p-0.5"
							>
								<span class="size-[15px] rounded-full bg-text-faint"></span>
							</span>
						</span>
					</div>
				{/each}
			</section>

			<section
				data-notification-band="delivery"
				class="flex h-[430px] w-[1310px] shrink-0 items-start gap-5"
				aria-label="Quiet hours and delivery preview"
			>
				<article
					class="flex h-[292px] w-[860px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-surface p-5"
				>
					<div class="flex h-[22px] shrink-0 items-baseline justify-between">
						<h3 class="text-lg leading-[22px] font-semibold">Quiet hours</h3>
						<span class="font-mono text-2xs leading-[14px] text-text-faint">POLICY UNAVAILABLE</span
						>
					</div>
					<div class="flex h-[52px] shrink-0 flex-col gap-1.5">
						<div
							class="grid h-[34px] place-items-center rounded-sm border border-dashed border-hairline-strong font-mono text-2xs tracking-caps text-text-faint"
						>
							NO QUIET-HOURS CONTRACT
						</div>
						<div class="flex justify-between font-mono text-[10px] leading-3 text-text-faint">
							<span>00:00</span><span>06:00</span><span>12:00</span><span>18:00</span><span
								>24:00</span
							>
						</div>
					</div>
					{#each [['Machine integrations remain separate', 'SEE INTEGRATIONS'], ['Critical bypass policy', 'UNAVAILABLE'], ['Digest scheduling', 'UNAVAILABLE']] as row (row[0])}
						<div class="flex h-10 shrink-0 items-center justify-between border-t border-hairline">
							<span class="text-[13px] leading-4 text-text-muted">{row[0]}</span>
							<span class="font-mono text-xs leading-4 text-text-faint">{row[1]}</span>
						</div>
					{/each}
				</article>

				<article
					class="flex h-[430px] w-[430px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-raised p-5"
				>
					<h3 class="h-[22px] shrink-0 text-lg leading-[22px] font-semibold">
						What lands on the phone
					</h3>
					<div
						class="flex h-[302px] w-[390px] shrink-0 flex-col overflow-hidden rounded-md border border-hairline-strong bg-ground"
					>
						<div
							class="grid h-[150px] shrink-0 place-items-center bg-video font-mono text-2xs tracking-caps text-text-faint"
						>
							NO DELIVERY PREVIEW
						</div>
						<div class="flex h-[110px] shrink-0 flex-col gap-1.5 p-3.5">
							<p class="text-sm leading-[18px] font-semibold">Delivery history unavailable</p>
							<p class="text-[13px] leading-5 text-text-muted">
								No rule runtime can provide a destination, attachment, narrative, or delivery
								receipt.
							</p>
						</div>
						<div
							class="grid h-10 shrink-0 place-items-center border-t border-hairline font-mono text-2xs text-text-faint"
						>
							ACTIONS UNAVAILABLE
						</div>
					</div>
					<p class="h-9 shrink-0 text-xs-plus leading-[18px] text-text-faint">
						A phone preview requires real rule execution and delivery evidence; none is synthesized.
					</p>
				</article>
			</section>
		</div>
	</div>
</section>
