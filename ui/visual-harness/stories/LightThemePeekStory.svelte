<script lang="ts">
	import { onMount } from 'svelte';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import BellIcon from '@lucide/svelte/icons/bell';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import EyeIcon from '@lucide/svelte/icons/eye';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import PeekCameraTile from '$lib/components/PeekCameraTile.svelte';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type {
		CameraHealth,
		CameraHealthDimensions,
		CameraListItem,
		StreamHealthDimensions
	} from '$lib/types';

	setLivePeer();

	function camera(id: string, name: string): CameraListItem {
		return {
			id,
			ip: '192.0.2.1',
			name,
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
	}

	function health(
		id: string,
		name: string,
		state: CameraHealth['state'],
		options: {
			fps?: number;
			frames?: number;
			drops?: number;
			reportAgeMs: number;
			updatedAtMs: number;
			lastError?: string;
		}
	): CameraHealth {
		const reason =
			state === 'healthy'
				? 'healthy'
				: state === 'degraded'
					? 'ingress_drops'
					: 'transport_disconnected';
		const detail =
			state === 'healthy'
				? 'Transport, media, keyframe, and recording evidence is current'
				: state === 'degraded'
					? '14% frames dropped'
					: 'Camera transport is disconnected';
		const current = state === 'healthy' || state === 'degraded';
		return {
			id,
			ip: '192.0.2.1',
			name,
			manufacturer: null,
			model: null,
			firmware_version: null,
			state,
			reason,
			reason_codes: [reason],
			detail,
			dimensions: {
				transport_connected: state !== 'offline',
				frames_fresh: current,
				decodable: current,
				recording_requested: true,
				recording_progressing: current
			} as CameraHealthDimensions,
			lifecycle: state === 'offline' ? 'Stopped' : 'Connected',
			last_error: options.lastError ?? null,
			configured_profiles: [],
			streams: [
				{
					type: 'sub',
					fps: options.fps,
					frames: options.frames ?? 0,
					drops: options.drops ?? 0,
					updated_at_ms: options.updatedAtMs,
					report_age_ms: options.reportAgeMs,
					state,
					reason,
					reason_codes: [reason],
					detail,
					dimensions: {
						expected: true,
						transport_connected: state !== 'offline',
						report_fresh: options.reportAgeMs <= 30_000,
						frames_fresh: current,
						decodable: current,
						recording_requested: true,
						recording_progressing: current
					} as StreamHealthDimensions
				}
			]
		};
	}

	const cameras = [
		camera('front-door', 'Front Door'),
		camera('porch', 'Porch'),
		camera('back-yard', 'Back Yard')
	];
	const healthById = new Map<string, CameraHealth>([
		[
			'front-door',
			health('front-door', 'Front Door', 'healthy', {
				fps: 25,
				frames: 1_000,
				reportAgeMs: 20,
				updatedAtMs: Date.parse('2026-08-18T06:37:23Z')
			})
		],
		[
			'porch',
			health('porch', 'Porch', 'degraded', {
				fps: 11,
				frames: 86,
				drops: 14,
				reportAgeMs: 4_000,
				updatedAtMs: Date.parse('2026-08-18T06:37:19Z')
			})
		],
		[
			'back-yard',
			health('back-yard', 'Back Yard', 'offline', {
				reportAgeMs: 8_056_000,
				updatedAtMs: Date.parse('2026-08-18T04:23:07Z'),
				lastError: 'Not recording. No footage since 04:23.'
			})
		]
	]);
	const navigation = [EyeIcon, HistoryIcon, BellIcon, CameraIcon, ActivityIcon] as const;
	const frameNowMs = Date.parse('2026-08-18T06:37:23Z');

	onMount(() => {
		const root = document.documentElement;
		const previousTheme = root.dataset.theme;
		const wasDark = root.classList.contains('dark');
		root.classList.remove('dark');
		root.dataset.theme = 'light';
		return () => {
			root.classList.toggle('dark', wasDark);
			if (previousTheme === undefined) delete root.dataset.theme;
			else root.dataset.theme = previousTheme;
		};
	});
</script>

<main
	data-paper-scenario="peek.desktop.light-theme"
	class="flex h-[362px] w-[1440px] overflow-hidden rounded-lg border border-hairline-strong bg-ground"
>
	<aside
		class="flex h-[360px] w-16 shrink-0 flex-col items-center gap-2.5 border-r border-hairline bg-ground py-4"
		aria-label="Desktop navigation"
	>
		<a
			href="/"
			class="grid h-[30px] w-[34px] shrink-0 place-items-center rounded-sm bg-primary font-mono text-[10px] font-semibold text-on-primary"
			aria-label="Dashboard"
			aria-current="page"
		>
			KP
		</a>
		<div class="h-[18px] shrink-0"></div>
		{#each navigation as Icon, index (index)}
			<a
				href={index === 0 ? '/viewer' : '/'}
				class="relative grid h-11 w-16 shrink-0 place-items-center border-l-2 border-transparent text-text-faint"
				aria-label={['Viewer', 'Keep', 'Events', 'Cameras', 'Health'][index]}
			>
				<Icon class="size-5" strokeWidth={1.75} />
			</a>
		{/each}
	</aside>

	<div class="relative h-[360px] min-w-0 flex-1">
		<section class="absolute inset-0 flex gap-2 bg-ground p-2">
			{#each cameras as camera (camera.id)}
				<PeekCameraTile
					{camera}
					health={healthById.get(camera.id)}
					stream="sub"
					compactStatus
					compactNowMs={frameNowMs}
					compactTimeZone="UTC"
					onfocus={() => {}}
				/>
			{/each}
		</section>

		<header
			data-peek-dashboard-switcher
			class="absolute top-3 left-3 z-30 flex h-8 items-center overflow-hidden rounded-sm border border-hairline-strong bg-surface/90 shadow-md backdrop-blur-md"
		>
			<h1
				class="flex h-full items-center border-r border-hairline px-2 font-mono text-[9px] font-semibold text-text-faint"
			>
				PEEK
			</h1>
			<button type="button" class="h-full shrink-0 px-2.5 text-xs font-medium text-foreground">
				All cameras
			</button>
		</header>
	</div>
</main>
