<script lang="ts">
	import { setAppearanceState } from '$lib/appearance-context';
	import { setCapabilityState } from '$lib/capability-context';
	import MobileAccessSection from '$lib/components/MobileAccessSection.svelte';
	import MobileNavigation from '$lib/components/MobileNavigation.svelte';
	import MobileSettingsActionBar from '$lib/components/MobileSettingsActionBar.svelte';
	import MobileSettingsHeader from '$lib/components/MobileSettingsHeader.svelte';
	import MobileSettingsIndex from '$lib/components/MobileSettingsIndex.svelte';
	import type { SanitizedConfig } from '$lib/types';
	import MobileDeviceStatusBar from './MobileDeviceStatusBar.svelte';

	type State = 'access' | 'index';
	type Props = { state?: State };

	let { state = 'index' }: Props = $props();
	setAppearanceState();
	setCapabilityState();

	const scenarioIds: Record<State, string> = {
		access: 'settings.mobile.access',
		index: 'settings.mobile.administration'
	};

	const config = {
		host: '0.0.0.0',
		port: 3000,
		camera_count: 42,
		storage: {
			medium_term_path: '/recordings/medium',
			long_term_path: '/recordings/long',
			recording_catalog_path: '/recordings/long/recordings.db',
			event_thumbnail_path: '/recordings/long/.event-thumbnails',
			event_thumbnail_max_mb: 1024,
			short_term_secs: 120,
			medium_term_secs: 1800,
			flush_interval_secs: 60,
			write_buffer_bytes: 8192,
			long_term_max_gb: 2048
		},
		recording_estimate: {
			estimated_bitrate_bps: 8_576_000,
			bytes_per_day: 92_620_800_000,
			known_streams: 42,
			unknown_streams: 0,
			estimated_retention_days: 11
		}
	} satisfies SanitizedConfig;
</script>

<main
	data-paper-scenario={scenarioIds[state]}
	class="flex h-[844px] w-[390px] flex-col overflow-hidden rounded-lg border border-hairline-strong bg-ground [font-synthesis:none]"
>
	<MobileDeviceStatusBar />
	{#if state === 'index'}
		<MobileSettingsHeader title="More" />
		<div class="min-h-0 flex-1 overflow-hidden">
			<MobileSettingsIndex {config} />
		</div>
		<MobileNavigation pathname="/settings" fixed={false} />
	{:else}
		<MobileSettingsHeader title="Access" backHref="/settings" trailing="Target · identity v1" />
		<MobileAccessSection />
		<MobileSettingsActionBar action="New token" capability="keeppeek.identity.v1" fixed={false} />
	{/if}
</main>
