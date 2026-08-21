<script lang="ts">
	import type { PtzPreset } from '$lib/control-client';
	import type { CameraHealth, CameraListItem, ProfileSummary } from '$lib/types';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import LockIcon from '@lucide/svelte/icons/lock';
	import CameraOverview from './CameraOverview.svelte';
	import DesktopPaperRail from './DesktopPaperRail.svelte';

	type Props = {
		camera: CameraListItem;
		health: CameraHealth;
		presets: readonly PtzPreset[];
	};

	let { camera, health, presets }: Props = $props();
	const anchors = [
		'Live & control',
		'Connection',
		'Streams & roles',
		'Recording',
		'Events',
		'Audio',
		'Advanced'
	] as const;
	let mainProfile = $derived(camera.profiles.find((profile) => profile.stream === 'main') ?? null);
	let subProfile = $derived(camera.profiles.find((profile) => profile.stream === 'sub') ?? null);
	let profileRows = $derived([
		{ label: 'Main', profile: mainProfile },
		{ label: 'Sub', profile: subProfile }
	]);

	function profileLabel(profile: ProfileSummary | null): string {
		if (!profile) return 'Not reported';
		return [
			profile.resolution,
			profile.framerate ? `${profile.framerate} FPS` : null,
			profile.encoding
		]
			.filter(Boolean)
			.join(' · ');
	}
</script>

<main
	data-camera-paper-frame
	class="flex h-[2059px] w-[1440px] shrink-0 overflow-hidden rounded-lg border border-hairline bg-ground [font-synthesis:none]"
>
	<DesktopPaperRail active="cameras" paperFull />

	<section class="flex h-[2057px] w-[1374px] shrink-0 flex-col" aria-label="Camera page evidence">
		<header
			data-camera-context-bar
			class="flex h-[52px] w-[1374px] shrink-0 items-center gap-3 border-b border-hairline px-5"
		>
			<span class="text-[13px] leading-4 text-text-muted">Cameras</span>
			<ChevronRightIcon class="size-3 text-text-faint" />
			<h1 class="text-base leading-5 font-semibold">{camera.name ?? camera.id}</h1>
			<span
				class="inline-flex h-[26px] items-center gap-[7px] rounded-full border border-hairline bg-raised px-2.5 text-xs"
			>
				<span class="size-1.5 rounded-full bg-healthy"></span>{health.state === 'online'
					? 'Connected'
					: health.state}
			</span>
			<span class="font-mono text-[11px] leading-[14px] text-text-muted">
				{camera.ip} · {camera.manufacturer ?? 'MANUFACTURER NOT REPORTED'}
				{camera.model ?? ''}
			</span>
			<span class="flex-1"></span>
			<button
				type="button"
				class="h-[30px] rounded-sm border border-hairline bg-raised px-3 text-[13px] text-text-muted"
			>
				Test connection
			</button>
			<button
				type="button"
				class="h-[30px] rounded-sm bg-hairline px-3 text-[13px] font-medium text-text-faint"
				disabled
				title="Broad camera save is unavailable"
			>
				Save unavailable
			</button>
		</header>

		<div data-camera-paper-body class="flex h-[2005px] w-[1374px] shrink-0">
			<aside
				data-camera-anchor-rail
				class="flex h-[2005px] w-[196px] shrink-0 flex-col gap-0.5 border-r border-hairline bg-surface px-3 py-4"
			>
				<p class="px-2.5 pb-2.5 font-mono text-[10px] leading-3 tracking-[0.14em] text-text-faint">
					ON THIS PAGE
				</p>
				{#each anchors as anchor, index (anchor)}
					<span
						class="flex h-[34px] shrink-0 items-center gap-2.5 rounded-sm px-2.5 text-[13px] {index ===
						0
							? 'bg-raised font-medium text-foreground'
							: 'text-text-muted'}"
					>
						<span class="h-3.5 w-0.5 {index === 0 ? 'bg-primary' : ''}"></span>{anchor}
					</span>
				{/each}
				<span class="flex-1"></span>
				<span
					class="flex h-[35px] shrink-0 items-center border-t border-hairline px-2.5 text-[13px] text-live-text"
					>Remove unavailable</span
				>
			</aside>

			<div data-camera-paper-content class="flex h-[2005px] w-[1178px] shrink-0 flex-col gap-7 p-6">
				<CameraOverview
					{camera}
					{health}
					stream="main"
					previewAvailable={false}
					commandTransportAvailable
					paperFrame
				/>

				<section
					data-camera-paper-section="presets"
					class="flex h-[154px] w-[1130px] shrink-0 flex-col gap-3.5"
				>
					<header class="flex h-[30px] shrink-0 items-center gap-3">
						<h2 class="text-[15px] leading-[18px] font-semibold">Presets</h2>
						<p class="text-[13px] leading-4 text-text-muted">
							Positions returned by the camera. Recall uses WebRTC control.
						</p>
						<span class="flex-1"></span>
						<button
							type="button"
							class="h-[30px] rounded-sm border border-hairline-strong bg-raised px-[11px] text-xs text-text-faint"
							disabled
						>
							Save unavailable
						</button>
					</header>
					<div class="flex h-[110px] shrink-0 gap-2.5">
						{#each Array.from({ length: 5 }) as _, index (index)}
							<div
								class="flex h-[110px] min-w-0 flex-1 flex-col gap-2 rounded-md border p-2.5 {index <
								presets.length
									? index === 0
										? 'border-primary bg-surface'
										: 'border-hairline bg-surface'
									: 'border-dashed border-hairline-strong bg-surface'}"
							>
								{#if presets[index]}
									<span class="h-16 shrink-0 rounded-sm bg-video"></span>
									<span class="text-xs font-medium">{presets[index].name}</span>
								{:else}
									<span class="grid h-full place-items-center text-xs text-text-faint"
										>Empty slot</span
									>
								{/if}
							</div>
						{/each}
					</div>
				</section>

				<section
					data-camera-paper-section="streams"
					class="flex h-[217px] w-[1130px] shrink-0 flex-col gap-4 border-t border-hairline pt-6"
				>
					<header class="flex h-6 shrink-0 items-baseline gap-3">
						<h2 class="text-xl leading-6 font-semibold">Streams & roles</h2>
						<p class="text-[13px] leading-4 text-text-muted">
							Discovered media evidence; role writes are unavailable.
						</p>
					</header>
					{#each profileRows as row (row.label)}
						<div
							class="flex h-[68px] shrink-0 items-center gap-4 rounded-md border border-hairline bg-surface px-4 py-3.5"
						>
							<div class="flex w-[300px] shrink-0 flex-col gap-1.5">
								<span class="text-[13px] font-semibold">{row.label}</span>
								<span class="truncate font-mono text-[11px] text-text-muted"
									>{profileLabel(row.profile)}</span
								>
							</div>
							<span class="font-mono text-[10px] text-text-muted"
								>{row.profile?.audio ? 'AUDIO REPORTED' : 'NO AUDIO PROFILE'}</span
							>
							<span class="flex-1"></span>
							<span class="text-xs text-text-faint">Role assignment unavailable</span>
						</div>
					{/each}
				</section>

				<section
					data-camera-paper-section="recording"
					class="flex h-[263px] w-[1130px] shrink-0 flex-col gap-4 border-t border-hairline pt-6"
				>
					<header class="flex h-6 shrink-0 items-baseline gap-3">
						<h2 class="text-xl leading-6 font-semibold">Recording</h2>
						<p class="text-[13px] leading-4 text-text-muted">
							Per-camera retention, mode, and inheritance are not returned.
						</p>
					</header>
					{#each [['Retention', 'Not reported by the camera API'], ['Mode', 'Recorder policy is not exposed per camera']] as row (row[0])}
						<div
							class="flex min-h-[89px] flex-1 items-start gap-6 rounded-md border border-hairline bg-surface p-4"
						>
							<div class="w-[280px] shrink-0">
								<p class="text-[13px] font-semibold">{row[0]}</p>
								<p class="mt-1 text-xs leading-[17px] text-text-muted">{row[1]}</p>
							</div>
							<span class="font-mono text-xs text-text-faint">UNAVAILABLE</span>
						</div>
					{/each}
				</section>

				<section
					data-camera-paper-section="connection"
					class="flex h-[199px] w-[1130px] shrink-0 flex-col gap-4 border-t border-hairline pt-6"
				>
					<header class="flex h-6 shrink-0 items-baseline gap-3">
						<h2 class="text-xl leading-6 font-semibold">Connection</h2>
						<p class="text-[13px] leading-4 text-text-muted">
							Current discovered and configured transport evidence.
						</p>
					</header>
					<div class="grid h-[59px] shrink-0 grid-cols-[360px_150px_588px] gap-4">
						{#each [['Address', camera.ip], ['ONVIF port', String(camera.ports?.onvif ?? 'Not reported')], ['Sign-in', 'Credentials are write-only and not returned']] as field (field[0])}
							<div class="flex min-w-0 flex-col gap-1.5">
								<span class="text-[13px] text-text-muted">{field[0]}</span><span
									class="flex h-[37px] items-center truncate rounded-sm border border-hairline-strong bg-raised px-3 font-mono text-[13px]"
									>{field[1]}</span
								>
							</div>
						{/each}
					</div>
					<div class="flex h-[59px] shrink-0 gap-4">
						{#each [['Backend', camera.backend ?? 'Not reported'], ['Transport', camera.transport ?? 'Not reported'], ['Probe resolution', 'Not returned']] as field (field[0])}
							<div class="flex min-w-0 flex-1 flex-col gap-[7px]">
								<span class="text-xs font-medium text-text-muted">{field[0]}</span><span
									class="font-mono text-xs text-text-faint">{field[1]}</span
								>
							</div>
						{/each}
					</div>
				</section>

				<section
					data-camera-paper-section="events"
					class="flex h-[292px] w-[1130px] shrink-0 flex-col gap-4 border-t border-hairline pt-6"
				>
					<header class="flex h-6 shrink-0 items-baseline gap-3">
						<h2 class="text-xl leading-6 font-semibold">Events</h2>
						<p class="text-[13px] leading-4 text-text-muted">
							Stored events expose source categories, not a publisher registry.
						</p>
					</header>
					{#each [['Camera event source', 'Device category; connection identity not returned'], ['KeepPeek event pipeline', 'Pipeline category; publisher identity not returned'], ['External publishers', 'Registry, tokens, mappings, and heartbeat unavailable']] as row, index (row[0])}
						<div
							class="flex h-[65px] shrink-0 items-center gap-4 rounded-md border bg-surface px-4 py-3.5 {index ===
							2
								? 'border-activity'
								: 'border-hairline'}"
						>
							<div class="w-[260px] shrink-0">
								<p class="text-[13px] font-semibold">{row[0]}</p>
								<p class="mt-1 font-mono text-[11px] text-text-muted">{row[1]}</p>
							</div>
							<span class="flex-1"></span><span class="text-xs text-text-faint"
								>No runtime evidence</span
							>
						</div>
					{/each}
				</section>

				<section
					data-camera-paper-section="audio"
					class="flex h-[131px] w-[1130px] shrink-0 flex-col gap-4 border-t border-hairline pt-6"
				>
					<header class="flex h-6 shrink-0 items-baseline gap-3">
						<h2 class="text-xl leading-6 font-semibold">Audio</h2>
						<p class="text-[13px] leading-4 text-text-muted">
							Profile evidence is visible; record and talk commands are unavailable.
						</p>
					</header>
					<div class="flex h-[66px] shrink-0 gap-4">
						{#each [['Audio profile', mainProfile?.audio ? `${mainProfile.audio.encoding} · ${mainProfile.audio.sample_rate ?? '—'} Hz` : 'Not reported'], ['Two-way audio', camera.capabilities?.two_way_audio ? 'Capability reported · command unavailable' : 'Not reported']] as row (row[0])}
							<div
								class="flex min-w-0 flex-1 items-center gap-3.5 rounded-md border border-hairline bg-surface px-4 py-3.5"
							>
								<div>
									<p class="text-[13px] font-semibold">{row[0]}</p>
									<p class="mt-1 text-xs text-text-muted">{row[1]}</p>
								</div>
								<span class="flex-1"></span><span class="font-mono text-xs text-text-faint"
									>READ ONLY</span
								>
							</div>
						{/each}
					</div>
				</section>

				<section
					data-camera-paper-section="advanced"
					class="flex h-[111px] w-[1130px] shrink-0 flex-col gap-4 border-t border-hairline pt-6"
				>
					<header class="flex h-6 shrink-0 items-baseline gap-3">
						<h2 class="text-xl leading-6 font-semibold">Advanced</h2>
						<p class="text-[13px] leading-4 text-text-muted">
							Only returned camera identity and profile fields are shown.
						</p>
					</header>
					<div
						class="flex h-[46px] shrink-0 items-center gap-3 rounded-md border border-hairline bg-surface px-4"
					>
						<LockIcon class="size-3.5 text-text-faint" /><span class="text-xs text-text-muted"
							>Manufacturer override · P2P UID presence · profile metadata</span
						><span class="flex-1"></span><span class="font-mono text-[11px] text-text-faint"
							>EVIDENCE ONLY</span
						>
					</div>
				</section>
			</div>
		</div>
	</section>
</main>
