<script lang="ts">
	import { onMount } from 'svelte';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import BellIcon from '@lucide/svelte/icons/bell';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import SearchIcon from '@lucide/svelte/icons/search';
	import VideoIcon from '@lucide/svelte/icons/video';
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
	const navigation = [VideoIcon, HistoryIcon, BellIcon, CameraIcon, ActivityIcon] as const;
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
			class="grid size-[30px] shrink-0 place-items-center rounded-sm bg-primary text-md leading-[18px] font-bold text-on-primary"
			aria-label="KeepPeek home"
		>
			K
		</a>
		<div class="h-[18px] shrink-0"></div>
		{#each navigation as Icon, index (index)}
			<a
				href="/"
				class="relative grid h-11 w-16 shrink-0 place-items-center {index === 0
					? 'border-l-2 border-primary bg-[#B7410E1F] text-primary-soft'
					: 'border-l-2 border-transparent text-text-faint'}"
				aria-label={['Peek', 'Keep', 'Events', 'Cameras', 'Health'][index]}
				aria-current={index === 0 ? 'page' : undefined}
			>
				<Icon class="size-5" strokeWidth={1.75} />
			</a>
		{/each}
	</aside>

	<div class="flex h-[360px] min-w-0 flex-1 flex-col">
		<header
			class="flex h-[52px] shrink-0 items-center gap-3 border-b border-hairline bg-surface px-5"
		>
			<h1 class="w-[47px] shrink-0 text-xl leading-6 font-semibold">Peek</h1>
			<span class="w-[53px] shrink-0 text-sm leading-4 text-text-muted">Live view</span>
			<span class="h-4 w-px bg-hairline-strong"></span>
			<button
				type="button"
				class="h-7 w-[105px] shrink-0 rounded-sm border border-hairline bg-raised px-2.5 text-sm leading-4"
			>
				Front of house
			</button>
			<label
				class="ml-auto flex h-[34px] w-[210px] items-center gap-2 rounded-sm border border-hairline bg-raised px-3 text-text-faint"
			>
				<SearchIcon class="size-[13px] shrink-0" strokeWidth={1.75} />
				<input
					type="search"
					placeholder="Search cameras"
					class="min-w-0 flex-1 bg-transparent text-sm leading-4 outline-none placeholder:text-text-faint"
				/>
				<kbd
					class="grid h-5 min-w-[26px] place-items-center rounded-sm border border-hairline-strong bg-surface px-1 font-mono text-2xs leading-3 text-text-muted"
					>⌘K</kbd
				>
			</label>
		</header>

		<section class="flex h-[268px] shrink-0 gap-4 bg-ground p-4">
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

		<footer
			class="flex h-8 shrink-0 items-center gap-[18px] border-t border-hairline bg-surface px-5"
		>
			<span class="flex w-[164px] shrink-0 items-center gap-1.5 text-xs-plus leading-4">
				<span class="size-1.5 rounded-full bg-activity"></span>
				1 camera offline · 1 degraded
			</span>
			<span class="w-[47px] shrink-0 font-mono text-xs leading-[14px] text-text-muted">CPU 34%</span
			>
			<span class="w-[179px] shrink-0 font-mono text-xs leading-[14px] text-text-muted"
				>STORAGE 71% · 12d PROJECTED</span
			>
			<span
				class="ml-auto flex w-[103px] shrink-0 items-center gap-1.5 text-xs-plus leading-4 text-text-muted"
			>
				<span class="size-1.5 rounded-full bg-healthy"></span>
				Recorder healthy
			</span>
		</footer>
	</div>
</main>
