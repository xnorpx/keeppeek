<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { useControlClient } from '$lib/control-context';
	import FirstRunEmptyStates from '$lib/components/FirstRunEmptyStates.svelte';
	import FirstRunSetupPanel from '$lib/components/FirstRunSetupPanel.svelte';
	import InitialAccessKeyClaim from '$lib/components/InitialAccessKeyClaim.svelte';
	import { detectedBrowserTimeZone } from '$lib/first-run';
	import type { StorageWriteProbe } from '$lib/first-run';
	import type { SanitizedConfig, ServerHealthResponse } from '$lib/types';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';

	const controlClient = useControlClient();

	let config = $state.raw<SanitizedConfig | null>(null);
	let health = $state.raw<ServerHealthResponse | null>(null);
	let timeZone = $state<string | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let storageProbe = $state.raw<StorageWriteProbe | null>(null);
	let probingStorage = $state(false);
	let initialAccessKeyPending = $state(false);

	onMount(() => {
		timeZone = detectedBrowserTimeZone();
		void loadEvidence();
	});

	async function loadEvidence(): Promise<void> {
		loading = true;
		error = null;
		try {
			const [nextConfig, nextHealth, credentials] = await Promise.all([
				controlClient.getRuntimeConfiguration(),
				controlClient.getHealth(),
				controlClient.listAccessCredentials()
			]);
			config = nextConfig;
			health = nextHealth;
			initialAccessKeyPending = credentials.some(
				(credential) => credential.initialAccessKeyPending
			);
			await probeStorage(nextConfig.storage.medium_term_path);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'First-run evidence could not be loaded.';
		} finally {
			loading = false;
		}
	}

	async function probeStorage(path: string): Promise<void> {
		probingStorage = true;
		try {
			storageProbe = await controlClient.probeStorage(path);
		} catch (cause) {
			storageProbe = {
				writable: false,
				detail: cause instanceof Error ? cause.message : 'Storage write verification failed.'
			};
		} finally {
			probingStorage = false;
		}
	}

	function continueSetup(): void {
		void goto(config?.camera_count ? resolve('/cameras') : resolve('/cameras/new'));
	}

	async function claimInitialAccessKey(): Promise<string> {
		const accessKey = await controlClient.revealAccessKey();
		initialAccessKeyPending = false;
		return accessKey;
	}
</script>

<svelte:head>
	<title>First run - KeepPeek</title>
</svelte:head>

<div class="mx-auto w-full max-w-[1280px] space-y-3 px-4 py-3">
	<header class="flex flex-wrap items-end justify-between gap-3 border-b border-hairline pb-3">
		<div>
			<p class="font-mono text-2xs tracking-caps text-primary-soft">FIRST RUN · LOCAL ONLY</p>
			<h1 class="mt-1 text-2xl font-semibold">Start with evidence</h1>
			<p class="mt-1 max-w-3xl text-sm leading-5 text-text-muted">
				Storage and time stay local. Remote readiness requires a retrieved Administrator credential.
			</p>
		</div>
		{#if health}
			<span
				class="rounded-full border border-activity/50 bg-activity/10 px-3 py-1 font-mono text-2xs tracking-caps"
			>
				PROOF OF CONCEPT · {health.version}
			</span>
		{/if}
	</header>

	{#if loading}
		<section
			class="grid min-h-[34rem] place-items-center rounded-md border border-hairline bg-surface"
			aria-label="Loading first-run evidence"
		>
			<div class="flex items-center gap-2 font-mono text-xs text-text-muted">
				<RefreshCwIcon class="size-4 animate-spin" /> Reading local configuration
			</div>
		</section>
	{:else if error}
		<section
			class="grid min-h-80 place-items-center rounded-md border border-live/40 bg-live/5 p-6 text-center"
			role="alert"
		>
			<div class="max-w-lg space-y-4">
				<TriangleAlertIcon class="mx-auto size-7 text-live-text" />
				<div>
					<h2 class="text-base font-semibold">First-run evidence is unavailable</h2>
					<p class="mt-1 text-sm break-words text-text-muted">{error}</p>
				</div>
				<button
					type="button"
					class="inline-flex h-9 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-4 text-xs font-medium"
					onclick={() => void loadEvidence()}
				>
					<RefreshCwIcon class="size-3.5" /> Retry
				</button>
			</div>
		</section>
	{:else if config}
		<div class="grid items-start gap-4 lg:grid-cols-[minmax(0,1.18fr)_minmax(22rem,0.82fr)]">
			<FirstRunSetupPanel
				{config}
				{health}
				{timeZone}
				writeProbe={storageProbe}
				{probingStorage}
				onretryprobe={() => void probeStorage(config!.storage.medium_term_path)}
				onstart={continueSetup}
			/>
			<div class="space-y-4">
				<InitialAccessKeyClaim pending={initialAccessKeyPending} onclaim={claimInitialAccessKey} />
				<FirstRunEmptyStates cameraCount={config.camera_count} />
			</div>
		</div>
	{/if}
</div>
