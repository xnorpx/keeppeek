<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import FirstRunEmptyStates from '$lib/components/FirstRunEmptyStates.svelte';
	import FirstRunSetupPanel from '$lib/components/FirstRunSetupPanel.svelte';
	import InitialAccessKeyClaim from '$lib/components/InitialAccessKeyClaim.svelte';
	import type { StorageWriteProbe } from '$lib/first-run';
	import type { SanitizedConfig } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import CircleDotIcon from '@lucide/svelte/icons/circle-dot';

	setCapabilityState();

	const config = {
		host: '0.0.0.0',
		port: 8081,
		storage: {
			medium_term_path: '/mnt/keeppeek',
			long_term_path: '/mnt/keeppeek',
			recording_catalog_path: '/mnt/keeppeek/recordings.db',
			event_thumbnail_path: '/mnt/keeppeek/.event-thumbnails',
			event_thumbnail_max_mb: 512,
			short_term_secs: 10,
			medium_term_secs: 3600,
			flush_interval_secs: 5,
			write_buffer_bytes: 1_048_576,
			long_term_max_gb: 0
		},
		camera_count: 0,
		recording_estimate: {
			estimated_bitrate_bps: 0,
			bytes_per_day: 0,
			known_streams: 0,
			unknown_streams: 0,
			estimated_retention_days: null
		}
	} satisfies SanitizedConfig;

	const health = {
		version: 'v0.4.1-pre',
		system: {
			disks: [
				{
					name: 'recordings',
					kind: 'SSD',
					file_system: 'apfs',
					mount_point: '/mnt/keeppeek',
					total_bytes: 8_000_000_000_000,
					available_bytes: 7_900_000_000_000,
					used_bytes: 100_000_000_000,
					removable: false,
					stores_recordings: true
				}
			]
		}
	};

	const demoInitialAccessKey = '8f4e2a10-6c3b-4d91-9e52-7b1c4a8d3f20';

	let stage = $state<'setup' | 'ready'>('setup');
	let writeProbe = $state.raw<StorageWriteProbe | null>(null);
	let probingStorage = $state(false);
	let initialAccessKeyPending = $state(true);
	let initialKeyClaimed = $state(false);
	let probeTimer: ReturnType<typeof setTimeout> | null = null;

	$effect(() => {
		return () => {
			if (probeTimer !== null) {
				clearTimeout(probeTimer);
				probeTimer = null;
			}
		};
	});

	let checklist = $derived([
		{
			label: 'Storage write proof',
			done: writeProbe?.writable === true,
			detail: probingStorage
				? 'Running write, flush, rename, and cleanup probe'
				: writeProbe?.writable
					? '/mnt/keeppeek verified writable (7.9 TB free)'
					: 'Pending write verification before camera setup'
		},
		{
			label: 'Browser time zone notice',
			done: true,
			detail: 'Europe/Stockholm detected · host OS clock stays authoritative'
		},
		{
			label: 'Initial Administrator key',
			done: initialKeyClaimed,
			detail: initialKeyClaimed
				? 'Retrieved once and hidden permanently'
				: 'Remote access unready until retrieved and stored'
		}
	]);

	function runWriteProbe(): void {
		if (probingStorage) return;
		probingStorage = true;
		probeTimer = setTimeout(() => {
			probeTimer = null;
			writeProbe = {
				writable: true,
				detail: 'Write, flush, rename, and cleanup succeeded.'
			};
			probingStorage = false;
		}, 600);
	}

	async function claimInitialAccessKey(): Promise<string> {
		return demoInitialAccessKey;
	}

	function markInitialKeyClaimed(): void {
		initialAccessKeyPending = false;
		initialKeyClaimed = true;
	}

	function continueSetup(): void {
		if (writeProbe?.writable !== true) return;
		stage = 'ready';
	}
</script>

<main
	data-paper-scenario="setup.desktop.first-run"
	class="flex h-[800px] w-[1280px] flex-col overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<header
		class="flex h-[72px] shrink-0 items-center justify-between gap-4 border-b border-hairline bg-surface px-6"
	>
		<div>
			<p class="font-mono text-2xs tracking-caps text-primary-soft">
				{stage === 'setup' ? 'FIRST RUN · /SETUP · LOCAL ONLY' : 'DASHBOARD · EMPTY FLEET READY'}
			</p>
			<h1 class="mt-0.5 text-xl font-semibold">
				{stage === 'setup' ? 'Start with evidence' : 'Ready for your first camera'}
			</h1>
		</div>
		<div class="flex items-center gap-3">
			<span
				class="rounded-full border border-hairline-strong bg-raised px-3 py-1 font-mono text-2xs tracking-caps text-text-muted"
			>
				0 CAMERAS CONFIGURED
			</span>
			<span
				class="rounded-full border border-activity/50 bg-activity/10 px-3 py-1 font-mono text-2xs tracking-caps"
			>
				PROOF OF CONCEPT · {health.version}
			</span>
		</div>
	</header>

	<div class="min-h-0 flex-1 overflow-hidden px-6 py-4">
		{#if stage === 'setup'}
			<div class="grid h-full grid-cols-[minmax(0,1.18fr)_minmax(0,0.82fr)] items-start gap-5">
				<FirstRunSetupPanel
					{config}
					{health}
					timeZone="Europe/Stockholm"
					{writeProbe}
					{probingStorage}
					onretryprobe={runWriteProbe}
					onstart={continueSetup}
				/>

				<div class="flex flex-col gap-4">
					<InitialAccessKeyClaim
						pending={initialAccessKeyPending}
						onclaim={claimInitialAccessKey}
						onclaimed={markInitialKeyClaimed}
					/>

					<section
						class="rounded-md border border-hairline bg-surface p-4"
						aria-labelledby="setup-readiness-heading"
					>
						<h2
							id="setup-readiness-heading"
							class="font-mono text-2xs tracking-caps text-text-faint"
						>
							FIRST-RUN EVIDENCE CHECKLIST
						</h2>
						<ul class="mt-3 space-y-2.5">
							{#each checklist as item (item.label)}
								<li
									class="flex items-start gap-2.5 rounded-sm border border-hairline bg-raised p-2.5"
								>
									<span
										class="mt-0.5 grid size-5 shrink-0 place-items-center rounded-full {item.done
											? 'bg-healthy/15 text-healthy'
											: 'bg-activity/15 text-activity'}"
									>
										{#if item.done}
											<CheckIcon class="size-3" />
										{:else}
											<CircleDotIcon class="size-3" />
										{/if}
									</span>
									<div class="min-w-0">
										<p class="text-xs font-semibold">{item.label}</p>
										<p class="mt-0.5 text-2xs leading-4 text-text-muted">{item.detail}</p>
									</div>
								</li>
							{/each}
						</ul>
					</section>
				</div>
			</div>
		{:else}
			<section
				data-demo-first-run-ready="true"
				class="mx-auto flex h-full max-w-4xl flex-col gap-4"
				aria-label="Empty fleet workspace"
			>
				<div
					class="flex items-center justify-between gap-4 rounded-md border border-healthy/40 bg-healthy/5 px-4 py-3"
					role="status"
				>
					<div class="flex items-center gap-3">
						<span
							class="grid size-7 shrink-0 place-items-center rounded-full bg-healthy/15 text-healthy"
						>
							<CheckIcon class="size-4" />
						</span>
						<div>
							<p class="text-sm font-semibold">First-run setup complete</p>
							<p class="text-xs text-text-muted">
								/mnt/keeppeek write proof verified · Initial Administrator key claimed · Browser
								time Europe/Stockholm
							</p>
						</div>
					</div>
					<span class="font-mono text-2xs tracking-caps text-healthy">STORAGE VERIFIED</span>
				</div>

				<FirstRunEmptyStates cameraCount={config.camera_count} emptyDayLabel="18 AUG" />
			</section>
		{/if}
	</div>
</main>
