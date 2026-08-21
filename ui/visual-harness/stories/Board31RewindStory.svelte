<script lang="ts">
	import { onMount } from 'svelte';
	import PeekCameraTile from '$lib/components/PeekCameraTile.svelte';
	import PeekRewindState from '$lib/components/PeekRewindState.svelte';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type { CameraHealth, CameraListItem } from '$lib/types';

	type State = 'ready' | 'scrubbing';
	type Props = {
		state: State;
		onrewind?: (cameraId: string, targetTimestampMs: number) => void | Promise<void>;
	};

	let { state, onrewind = () => {} }: Props = $props();
	setLivePeer();

	const camera: CameraListItem = {
		id: 'front-door',
		ip: '192.0.2.1',
		name: 'Front Door',
		manufacturer: null,
		model: null,
		firmware_version: null,
		is_reolink: false,
		capabilities: {
			ptz: false,
			audio: false,
			events: true,
			recording: true,
			analytics: false,
			imaging: false,
			two_way_audio: false
		},
		profiles: []
	};
	const health: CameraHealth = {
		id: camera.id,
		ip: camera.ip,
		name: camera.name ?? camera.id,
		manufacturer: null,
		model: null,
		firmware_version: null,
		state: 'online',
		lifecycle: 'Connected',
		last_error: null,
		configured_profiles: [],
		streams: [
			{
				type: 'sub',
				fps: 25,
				frames: 1_000,
				drops: 0,
				updated_at_ms: Date.parse('2026-08-18T06:37:23Z'),
				report_age_ms: 0
			}
		]
	};

	onMount(() => {
		const root = document.documentElement;
		const previousTheme = root.dataset.theme;
		const wasDark = root.classList.contains('dark');
		root.classList.add('dark');
		root.dataset.theme = 'dark';
		return () => {
			root.classList.toggle('dark', wasDark);
			if (previousTheme === undefined) delete root.dataset.theme;
			else root.dataset.theme = previousTheme;
		};
	});
</script>

<main
	data-paper-scenario={state === 'ready'
		? 'peek.desktop.rewind-ready'
		: 'peek.desktop.rewind-to-keep'}
	class="h-[262px] w-[464px] overflow-hidden [font-synthesis:none]"
>
	{#if state === 'ready'}
		<PeekCameraTile
			{camera}
			{health}
			stream="sub"
			compactStatus
			compactLiveBorder="hairline"
			compactNowMs={health.streams[0].updated_at_ms}
			compactTimeZone="UTC"
			rewindNowMsOverride={health.streams[0].updated_at_ms}
			rewindTimeZone="UTC"
			rewindControlVisible
			onfocus={() => {}}
			{onrewind}
		/>
	{:else}
		<PeekRewindState
			rewindSeconds={38}
			targetTimeLabel="06:36:45"
			markerPercent={(236 / 436) * 100}
			class="h-[262px] w-[464px]"
		/>
	{/if}
</main>
