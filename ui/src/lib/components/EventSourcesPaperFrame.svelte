<script lang="ts">
	import { resolve } from '$app/paths';
	import { eventSourceEvidence } from '$lib/event-sources';
	import type { ServerHealthResponse } from '$lib/types';
	import DesktopPaperRail from './DesktopPaperRail.svelte';
	import SettingsPaperAnchorRail from './SettingsPaperAnchorRail.svelte';

	type Props = {
		health: ServerHealthResponse | null;
	};

	let { health }: Props = $props();
	let evidence = $derived(eventSourceEvidence(health));

	function formatCount(value: number | null | undefined): string {
		return value === null || value === undefined
			? 'Unavailable'
			: new Intl.NumberFormat('en-US').format(value);
	}
</script>

<section
	data-event-sources-paper-frame
	class="flex h-[1048px] w-[1440px] overflow-hidden rounded-lg border border-hairline bg-surface [font-synthesis:none]"
	aria-label="Event source evidence"
>
	<DesktopPaperRail />

	<div class="flex h-[1046px] w-[1374px] shrink-0 flex-col">
		<header
			class="flex h-[52px] w-[1374px] shrink-0 items-center justify-between border-b border-hairline px-5"
		>
			<div class="flex items-baseline gap-3">
				<h2 class="text-base leading-5 font-semibold">Settings</h2>
				<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-muted">
					EVENT SOURCES
				</p>
			</div>
			<button
				type="button"
				class="h-[30px] rounded-sm bg-primary px-3.5 text-[13px] font-semibold text-on-primary disabled:opacity-45"
				disabled
			>
				Register a source
			</button>
		</header>

		<div class="flex h-[994px] shrink-0">
			<SettingsPaperAnchorRail active="event-sources" />
			<div class="flex h-[994px] w-[1134px] shrink-0 flex-col gap-7 px-8 py-7">
				<section
					data-event-source-band="heading"
					class="flex h-[84px] w-[1070px] shrink-0 items-end justify-between"
					aria-labelledby="paper-event-sources-heading"
				>
					<div class="flex h-[84px] w-[720px] shrink-0 flex-col gap-1.5">
						<h1 id="paper-event-sources-heading" class="text-[28px] leading-[34px] font-semibold">
							Event sources
						</h1>
						<p class="text-sm leading-[22px] text-text-muted">
							Stored event evidence exists, but publisher identities, tokens, scopes, heartbeats,
							and vocabulary mappings are not exposed by this server.
						</p>
					</div>
					<div class="flex h-[58px] shrink-0 flex-col items-end gap-0.5">
						<p class="text-[40px] leading-[42px] font-bold">
							{formatCount(evidence.catalog?.totalEvents)}
						</p>
						<p class="font-mono text-2xs leading-[14px] tracking-[0.1em] text-text-faint">
							ALL-TIME CATALOG EVENTS
						</p>
					</div>
				</section>

				<section
					data-event-source-band="sources"
					class="flex h-[337px] w-[1070px] shrink-0 flex-col gap-3.5"
					aria-label="Source evidence"
				>
					<article
						class="flex h-[181px] shrink-0 flex-col overflow-hidden rounded-md border border-hairline-strong bg-surface"
					>
						<header
							class="flex h-[71px] shrink-0 items-center justify-between border-b border-hairline px-[18px] py-4"
						>
							<div class="flex items-center gap-3">
								<span class="size-2 rounded-full bg-activity"></span>
								<div class="flex flex-col gap-0.5">
									<h2 class="text-lg-plus leading-[22px] font-semibold">
										Source registry unavailable
									</h2>
									<p class="font-mono text-2xs leading-[14px] text-text-faint">
										NO LIST, REGISTRATION, HEARTBEAT, OR PUBLICATION HANDLER
									</p>
								</div>
							</div>
							<div class="flex gap-5 text-right">
								<div>
									<p class="font-mono text-sm">{formatCount(evidence.catalog?.openEvents)}</p>
									<p class="font-mono text-[10px] tracking-[0.08em] text-text-faint">OPEN EVENTS</p>
								</div>
								<div>
									<p class="font-mono text-sm">{formatCount(evidence.catalog?.thumbnails)}</p>
									<p class="font-mono text-[10px] tracking-[0.08em] text-text-faint">THUMBNAILS</p>
								</div>
							</div>
						</header>
						<div class="flex h-[108px] shrink-0 gap-8 px-[18px] py-4">
							<div class="flex w-[300px] shrink-0 flex-col gap-2">
								<p class="font-mono text-2xs tracking-[0.14em] text-text-faint">
									AVAILABLE EVIDENCE
								</p>
								<div class="flex flex-wrap gap-1.5">
									{#each ['all-time event count', 'open event count', 'thumbnail count'] as item (item)}
										<span
											class="rounded-xs border border-hairline bg-raised px-2 py-1 text-xs text-text-muted"
											>{item}</span
										>
									{/each}
								</div>
							</div>
							<div class="flex w-[280px] shrink-0 flex-col gap-2">
								<p class="font-mono text-2xs tracking-[0.14em] text-text-faint">NOT EVIDENCE</p>
								<p class="text-[13px] leading-[21px] text-text-muted">
									Counts do not identify a publisher, connection, token, permission, or event
									vocabulary.
								</p>
							</div>
							<div class="flex min-w-0 flex-1 flex-col gap-2">
								<p class="font-mono text-2xs tracking-[0.14em] text-text-faint">ADMINISTRATION</p>
								<p class="text-[13px] leading-[21px] text-text-muted">
									No management request is attempted from this screen.
								</p>
								<button
									type="button"
									class="h-[30px] self-start rounded-sm border border-hairline px-3 text-[13px] text-text-faint"
									disabled>Manage unavailable</button
								>
							</div>
						</div>
					</article>

					{#each [['camera', 'Persisted origin category for configured camera event paths.'], ['keeppeek', 'Persisted origin category for KeepPeek-side event processing.']] as origin (origin[0])}
						<article
							data-event-origin={origin[0]}
							class="flex h-16 shrink-0 items-center justify-between rounded-md border border-hairline bg-surface px-[18px] py-3.5"
						>
							<div class="flex w-[340px] shrink-0 items-center gap-3">
								<span class="size-2 rounded-full bg-healthy"></span>
								<div>
									<h3 class="text-[15px] leading-[18px] font-semibold">{origin[0]}</h3>
									<p class="font-mono text-2xs text-text-faint">PERSISTED ORIGIN · NOT IDENTITY</p>
								</div>
							</div>
							<p class="w-[520px] shrink-0 text-[13px] leading-[21px] text-text-muted">
								{origin[1]}
							</p>
							<a
								href={resolve('/events')}
								class="inline-flex h-[30px] items-center rounded-sm border border-hairline-strong px-3 text-[13px]"
								>Browse events</a
							>
						</article>
					{/each}
				</section>

				<section
					data-event-source-band="mapping"
					class="flex h-[461px] w-[1070px] shrink-0 items-start gap-5"
					aria-label="Event field and mapping evidence"
				>
					<article
						class="flex h-[295px] w-[690px] shrink-0 flex-col rounded-md border border-hairline bg-surface p-[18px]"
					>
						<header class="flex h-[61px] shrink-0 flex-col gap-1 pb-3.5">
							<h2 class="text-lg leading-[22px] font-semibold">Vocabulary mapping unavailable</h2>
							<p class="text-[13px] leading-[21px] text-text-muted">
								The current API stores raw event kind strings and exposes no source-specific mapping
								table.
							</p>
						</header>
						<div
							class="flex h-7 shrink-0 items-center border-b border-hairline-strong font-mono text-2xs tracking-[0.14em] text-text-faint"
						>
							<span class="w-[200px]">SOURCE FIELD</span><span class="w-6"></span><span
								class="w-[180px]">SHOWS AS</span
							><span class="w-[106px]">COLOUR</span><span>SWIMLANE</span>
						</div>
						{#each ['Publisher registry', 'Type mapping', 'Colour mapping', 'Swimlane mapping'] as row (row)}
							<div
								class="flex h-[42px] shrink-0 items-center border-b border-hairline text-[13px] text-text-muted"
							>
								<span class="w-[200px] font-mono text-xs">{row}</span><span class="w-6">→</span
								><span class="w-[180px]">Unavailable</span><span
									class="w-[106px] font-mono text-xs text-text-faint">—</span
								><span class="text-text-faint">—</span>
							</div>
						{/each}
					</article>

					<article
						class="flex h-[461px] w-[360px] shrink-0 flex-col gap-3.5 rounded-md border border-hairline bg-raised p-[18px]"
					>
						<header class="flex h-[89px] shrink-0 flex-col gap-1">
							<h2 class="text-lg leading-[22px] font-semibold">What stored events carry</h2>
							<p class="text-[13px] leading-[21px] text-text-muted">
								Fields below are exposed by current stored-media WebRTC responses, not by an HTTP
								event endpoint or Paper-only publisher payloads.
							</p>
						</header>
						<div class="flex flex-col">
							{#each evidence.storedMediaFields.exposed.slice(0, 9) as field (field)}
								<div
									class="flex h-[30px] shrink-0 items-center justify-between border-b border-hairline"
								>
									<code class="font-mono text-xs">{field}</code><span
										class="text-xs text-text-faint">returned</span
									>
								</div>
							{/each}
						</div>
						<p class="text-xs-plus leading-[18px] text-text-faint">
							Not exposed in the UI model: {evidence.storedMediaFields.notExposed.join(', ')}
						</p>
					</article>
				</section>
			</div>
		</div>
	</div>
</section>
