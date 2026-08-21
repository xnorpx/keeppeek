<script lang="ts">
	import { resolve } from '$app/paths';
	import { eventSourceEvidence } from '$lib/event-sources';
	import type { ServerHealthResponse } from '$lib/types';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import ScanLineIcon from '@lucide/svelte/icons/scan-line';
	import EventSourcesPaperFrame from './EventSourcesPaperFrame.svelte';

	type Props = {
		health: ServerHealthResponse | null;
		healthError?: string | null;
		paperFrame?: boolean;
	};

	let { health, healthError = null, paperFrame = false }: Props = $props();
	let evidence = $derived(eventSourceEvidence(health));

	function formatCount(value: number | null | undefined): string {
		return value === null || value === undefined
			? 'Unavailable'
			: new Intl.NumberFormat().format(value);
	}
</script>

{#if paperFrame}
	<EventSourcesPaperFrame {health} />
{:else}
	<section
		id="event-sources"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
		aria-labelledby="event-sources-heading"
	>
		<header
			class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">EVENT SOURCES</p>
				<h2 id="event-sources-heading" class="mt-1 text-xl font-semibold">Event sources</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">
					KeepPeek persists camera and KeepPeek-origin events, but this server build exposes no
					publisher registry, token administration, permission scope, heartbeat, or type mapping
					API.
				</p>
			</div>
			<button
				type="button"
				class="inline-flex h-8 items-center gap-2 rounded-sm bg-primary px-3 text-xs font-semibold text-on-primary disabled:cursor-not-allowed disabled:opacity-45"
				disabled
				title="No event-source registration endpoint is implemented"
			>
				<PlusIcon class="size-3.5" /> Register a source
			</button>
		</header>

		<div class="grid border-b border-hairline lg:grid-cols-[0.8fr_1.2fr]">
			<div class="space-y-4 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div>
					<p class="font-mono text-2xs tracking-caps text-text-faint">CATALOG EVIDENCE</p>
					<div class="mt-3 grid grid-cols-3 gap-2 text-center">
						<div class="rounded-sm border border-hairline bg-raised px-2 py-3">
							<p class="text-lg font-semibold">{formatCount(evidence.catalog?.totalEvents)}</p>
							<p class="text-2xs text-text-faint">Total events</p>
						</div>
						<div class="rounded-sm border border-hairline bg-raised px-2 py-3">
							<p class="text-lg font-semibold">{formatCount(evidence.catalog?.openEvents)}</p>
							<p class="text-2xs text-text-faint">Open events</p>
						</div>
						<div class="rounded-sm border border-hairline bg-raised px-2 py-3">
							<p class="text-lg font-semibold">{formatCount(evidence.catalog?.thumbnails)}</p>
							<p class="text-2xs text-text-faint">Thumbnails</p>
						</div>
					</div>
				</div>
				<p
					class="flex gap-2 rounded-sm border border-hairline bg-raised px-3 py-3 text-xs leading-5 text-text-muted"
				>
					<DatabaseIcon class="mt-0.5 size-4 shrink-0 text-text-faint" />
					<span>
						{healthError ??
							'Catalog counts are all-time aggregates. The health response does not report events today, source breakdown, or last-event time.'}
					</span>
				</p>
				<a
					href={resolve('/events')}
					class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium"
				>
					<ScanLineIcon class="size-3.5" /> Browse stored event evidence
				</a>
			</div>

			<div class="space-y-3 p-5">
				<div class="flex flex-wrap items-baseline justify-between gap-2">
					<h3 class="text-base font-semibold">Persisted origin categories</h3>
					<span class="font-mono text-2xs tracking-caps text-text-faint"
						>NOT PUBLISHER IDENTITIES</span
					>
				</div>
				<div class="grid gap-3 sm:grid-cols-2">
					<article class="rounded-sm border border-hairline bg-raised p-4">
						<div class="flex items-center gap-2">
							<CameraIcon class="size-4 text-primary-soft" />
							<h4 class="text-sm font-semibold">camera</h4>
						</div>
						<p class="mt-2 text-xs leading-5 text-text-muted">
							Origin value for events emitted through a configured camera path. REST does not
							identify the camera protocol or publishing session behind it.
						</p>
					</article>
					<article class="rounded-sm border border-hairline bg-raised p-4">
						<div class="flex items-center gap-2">
							<ActivityIcon class="size-4 text-primary-soft" />
							<h4 class="text-sm font-semibold">keeppeek</h4>
						</div>
						<p class="mt-2 text-xs leading-5 text-text-muted">
							Origin value for KeepPeek-side event processing. It is not a service account, token,
							or external publisher name.
						</p>
					</article>
				</div>
				<div class="rounded-sm border border-activity/45 bg-activity/5 px-3 py-3" role="status">
					<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-activity">
						<CircleAlertIcon class="size-3.5" /> SOURCE REGISTRY UNAVAILABLE
					</div>
					<p class="mt-1.5 text-xs leading-5 text-text-muted">
						A populated event catalog cannot prove which external services are registered,
						connected, or permitted. Named cards such as object detectors and doorbell bridges
						therefore remain absent.
					</p>
				</div>
			</div>
		</div>

		<div class="grid lg:grid-cols-[1.15fr_0.85fr]">
			<div class="space-y-3 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div>
					<h3 class="text-base font-semibold">What stored WebRTC events carry</h3>
					<p class="mt-1 text-xs leading-5 text-text-muted">
						These normalized fields are exposed by the current per-camera, per-day stored-media
						query over WebRTC.
					</p>
				</div>
				<div class="flex flex-wrap gap-2">
					{#each evidence.storedMediaFields.exposed as field (field)}
						<code class="rounded-xs border border-hairline bg-raised px-2 py-1 text-2xs"
							>{field}</code
						>
					{/each}
				</div>
				<div class="border-t border-hairline pt-3">
					<p class="font-mono text-2xs tracking-caps text-text-faint">
						NOT EXPOSED IN THE UI MODEL
					</p>
					<div class="mt-2 flex flex-wrap gap-2">
						{#each evidence.storedMediaFields.notExposed as field (field)}
							<code
								class="rounded-xs border border-hairline bg-background px-2 py-1 text-2xs text-text-faint"
								>{field}</code
							>
						{/each}
					</div>
				</div>
			</div>

			<div class="space-y-4 p-5">
				<h3 class="text-base font-semibold">Administration contract</h3>
				<dl class="divide-y divide-hairline border-y border-hairline text-xs">
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="flex items-center gap-2 text-text-muted">
							<RadioIcon class="size-3.5" /> Connected publishers
						</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="flex items-center gap-2 text-text-muted">
							<KeyRoundIcon class="size-3.5" /> Tokens and last use
						</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Permissions and camera scope</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Event type mappings</dt>
						<dd class="font-mono text-text-faint">Unavailable</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="text-text-muted">WebRTC publication runtime</dt>
						<dd class="font-mono text-text-faint">Protocol types only</dd>
					</div>
				</dl>
				<div class="grid grid-cols-2 gap-2">
					<button
						type="button"
						class="h-8 rounded-sm border border-hairline bg-raised px-3 text-xs text-text-muted disabled:cursor-not-allowed"
						disabled>Manage source</button
					>
					<button
						type="button"
						class="h-8 rounded-sm border border-hairline bg-raised px-3 text-xs text-text-muted disabled:cursor-not-allowed"
						disabled>Rotate token</button
					>
				</div>
				<p class="text-xs leading-5 text-text-faint">
					No source-management request is attempted. Generated protobuf declarations alone do not
					prove a working server command path.
				</p>
			</div>
		</div>
	</section>
{/if}
