<script lang="ts">
	import type { AccessCredential } from '$lib/access';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import PeekDashboardAccessDialog from '$lib/components/PeekDashboardAccessDialog.svelte';
	import PeekLayoutEditor from '$lib/components/PeekLayoutEditor.svelte';
	import PeekWallSettings from '$lib/components/PeekWallSettings.svelte';
	import type { PeekLayout, PeekLayoutAudience, PeekLayoutDraft } from '$lib/peek-layout';
	import {
		defaultPeekWallPreferences,
		peekWallAppearancePreset,
		type PeekWallPreferences
	} from '$lib/peek-wall-preferences';
	import { setLivePeer } from '$lib/stream-peer-context';
	import type { CameraHealth, CameraListItem } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import LayoutGridIcon from '@lucide/svelte/icons/layout-grid';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import UsersIcon from '@lucide/svelte/icons/users';

	setLivePeer();

	const cameraDefs = [
		['front-door', 'Front Door', '#1e3a52'],
		['driveway', 'Driveway', '#243447'],
		['back-yard', 'Back Yard', '#1f3b32'],
		['side-gate', 'Side Gate', '#2d3145'],
		['garage', 'Garage', '#2e2b3f'],
		['workshop', 'Workshop', '#3a2428'],
		['yard-ptz', 'Yard PTZ', '#1b3847']
	] as const;

	const cameras: CameraListItem[] = cameraDefs.map(([id, name], index) => ({
		id,
		ip: `192.0.2.${index + 61}`,
		name,
		manufacturer: 'Reolink',
		model: `RLC-81${index + 1}A`,
		firmware_version: 'v3.1.0',
		is_reolink: true,
		profiles: []
	}));

	const accentByCameraId = new Map<string, string>(
		cameraDefs.map(([id, _name, accent]) => [id, accent])
	);

	const healthById = new Map<string, CameraHealth>(
		cameras.map(
			(camera) =>
				[
					camera.id,
					{
						id: camera.id,
						ip: camera.ip,
						name: camera.name ?? camera.id,
						manufacturer: camera.manufacturer,
						model: camera.model,
						firmware_version: camera.firmware_version,
						state: camera.id === 'workshop' ? 'offline' : 'healthy',
						lifecycle: camera.id === 'workshop' ? 'Stopped' : 'Connected',
						last_error: camera.id === 'workshop' ? 'Stream unavailable' : null,
						configured_profiles: [],
						streams: []
					}
				] as const
		)
	);

	const userCredentials: AccessCredential[] = [
		{
			id: '22222222-2222-4222-8222-222222222222',
			name: 'Front Desk Tablet',
			description: 'Reception wall monitor',
			role: 'user',
			createdAtMs: Date.parse('2026-08-16T08:00:00Z'),
			rotatedAtMs: null,
			lastUsedAtMs: Date.parse('2026-08-18T08:10:00Z'),
			expiresAtMs: null,
			disabled: false,
			revokedAtMs: null,
			revision: 1n,
			initialAccessKeyPending: false
		},
		{
			id: '33333333-3333-4333-8333-333333333333',
			name: 'Night Guard',
			description: 'Overnight gate viewer',
			role: 'user',
			createdAtMs: Date.parse('2026-08-18T08:15:00Z'),
			rotatedAtMs: null,
			lastUsedAtMs: null,
			expiresAtMs: null,
			disabled: false,
			revokedAtMs: null,
			revision: 1n,
			initialAccessKeyPending: false
		}
	];

	const initialLayout: PeekLayout = {
		id: 'front-of-house',
		name: 'Front of house',
		scope: 'shared',
		ownerId: '11111111-1111-4111-8111-111111111111',
		audience: { everyone: true, credentialIds: [] },
		activityFocus: true,
		items: [
			{ cameraId: 'front-door', column: 1, row: 1, columnSpan: 8, rowSpan: 8, pinned: false },
			{ cameraId: 'driveway', column: 9, row: 1, columnSpan: 4, rowSpan: 4, pinned: false },
			{ cameraId: 'back-yard', column: 9, row: 5, columnSpan: 4, rowSpan: 4, pinned: false }
		]
	};

	const deviceCapacity = 8;

	let mode = $state<'editor' | 'wall'>('editor');
	let savedLayout = $state.raw<PeekLayout>(initialLayout);
	let savedPreferences = $state.raw<PeekWallPreferences>(defaultPeekWallPreferences());
	let wallPreferences = $state.raw<PeekWallPreferences>(defaultPeekWallPreferences());
	let accessDialogOpen = $state(false);
	let layoutSavedOnce = $state(false);
	let wallSettingsSavedOnce = $state(false);
	let audienceSavedOnce = $state(false);

	let wallDirty = $derived(
		wallPreferences.tileShape !== savedPreferences.tileShape ||
			wallPreferences.mediaFit !== savedPreferences.mediaFit ||
			wallPreferences.streamingMode !== savedPreferences.streamingMode ||
			wallPreferences.streamLimit !== savedPreferences.streamLimit ||
			wallPreferences.keepAwake !== savedPreferences.keepAwake ||
			wallPreferences.gapPx !== savedPreferences.gapPx ||
			wallPreferences.cornerRadiusPx !== savedPreferences.cornerRadiusPx
	);

	let effectiveBudget = $derived(Math.min(deviceCapacity, wallPreferences.streamLimit));
	let activeStreams = $derived(Math.min(savedLayout.items.length, effectiveBudget));
	let appearancePresetLabel = $derived(peekWallAppearancePreset(wallPreferences).toUpperCase());
	let audienceLabel = $derived.by(() => {
		if (savedLayout.audience.everyone) return 'Everyone';
		const count = savedLayout.audience.credentialIds.length;
		if (count === 0) return 'Administrators only';
		return `${count} named ${count === 1 ? 'viewer' : 'viewers'}`;
	});
	let demoCompleted = $derived(layoutSavedOnce && wallSettingsSavedOnce && audienceSavedOnce);

	function handleSaveLayout(draft: PeekLayoutDraft): void {
		savedLayout = {
			...savedLayout,
			activityFocus: draft.activityFocus,
			items: draft.items.map((item) => ({ ...item }))
		};
		layoutSavedOnce = true;
		mode = 'wall';
	}

	function handleSaveWallSettings(): void {
		savedPreferences = { ...wallPreferences };
		wallSettingsSavedOnce = true;
	}

	function handleDiscardWallSettings(): void {
		wallPreferences = { ...savedPreferences };
	}

	async function handleSaveAudience(audience: PeekLayoutAudience): Promise<boolean> {
		savedLayout = {
			...savedLayout,
			audience: {
				everyone: audience.everyone,
				credentialIds: [...audience.credentialIds]
			}
		};
		audienceSavedOnce = true;
		return true;
	}

	function cameraLabel(cameraId: string): string {
		return cameras.find((camera) => camera.id === cameraId)?.name ?? cameraId;
	}
</script>

<main
	data-paper-scenario="peek.desktop.layout-editor"
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="dashboard" />

	{#if mode === 'editor'}
		<div class="min-h-0 min-w-0 flex-1 overflow-hidden p-3">
			<PeekLayoutEditor
				{cameras}
				{healthById}
				layout={savedLayout}
				persistenceAvailable
				streamFor={(camera) => (camera.id === 'front-door' ? 'main' : 'sub')}
				onsave={handleSaveLayout}
				ondiscard={() => (mode = 'wall')}
			/>
		</div>
	{:else}
		<div class="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
			<header
				class="flex h-14 shrink-0 items-center gap-3 border-b border-hairline bg-surface px-4"
			>
				<div
					class="flex items-center gap-2 rounded-sm border border-hairline bg-raised px-3 py-1.5"
				>
					<LayoutGridIcon class="size-3.5 text-primary-soft" />
					<h1 class="text-sm font-semibold">{savedLayout.name}</h1>
					<span class="font-mono text-2xs text-text-muted">{savedLayout.items.length} TILES</span>
				</div>

				<span class="font-mono text-2xs text-text-muted">
					{appearancePresetLabel} · {wallPreferences.gapPx}PX GAP · {wallPreferences.mediaFit.toUpperCase()}
					· LIMIT {wallPreferences.streamLimit}
				</span>

				<span class="flex-1"></span>

				<button
					type="button"
					data-demo-action="open-dashboard-access"
					class="inline-flex h-8 items-center gap-2 rounded-sm border border-hairline-strong bg-raised px-3 text-xs font-medium hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					onclick={() => (accessDialogOpen = true)}
				>
					<UsersIcon class="size-3.5 text-primary-soft" />
					Audience: {audienceLabel}
				</button>

				<button
					type="button"
					class="inline-flex h-8 items-center gap-1.5 rounded-sm border border-hairline bg-raised px-3 text-xs text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					onclick={() => (mode = 'editor')}
				>
					<PencilIcon class="size-3.5" /> Edit layout
				</button>

				{#if demoCompleted}
					<span
						data-demo-live-wall-complete="true"
						class="inline-flex items-center gap-1.5 rounded-full border border-healthy/40 bg-healthy/10 px-2.5 py-1 font-mono text-2xs text-healthy"
					>
						<CheckIcon class="size-3" /> LAYOUT · DISPLAY · AUDIENCE SAVED
					</span>
				{/if}
			</header>

			<div class="relative min-h-0 flex-1 overflow-hidden bg-ground p-3">
				<PeekWallSettings
					preferences={wallPreferences}
					{deviceCapacity}
					visibleStreams={savedLayout.items.length}
					{activeStreams}
					editable
					dirty={wallDirty}
					dashboardName={savedLayout.name}
					onchange={(next) => (wallPreferences = next)}
					onsave={handleSaveWallSettings}
					ondiscard={handleDiscardWallSettings}
				/>

				<div
					class="grid h-full w-full grid-cols-12 grid-rows-12"
					style:gap={`${wallPreferences.gapPx}px`}
					aria-label="Live camera wall"
				>
					{#each savedLayout.items as item, index (item.cameraId)}
						{@const health = healthById.get(item.cameraId)}
						{@const offline = health?.state === 'offline'}
						{@const overBudget = !offline && index >= effectiveBudget}
						{@const accent = accentByCameraId.get(item.cameraId) ?? '#1f2d3d'}
						<article
							class="relative min-h-0 min-w-0 overflow-hidden bg-video {wallPreferences.cornerRadiusPx ===
							0
								? ''
								: 'border border-hairline'}"
							style:grid-column={`${item.column} / span ${item.columnSpan}`}
							style:grid-row={`${item.row} / span ${item.rowSpan}`}
							style:border-radius={`${wallPreferences.cornerRadiusPx}px`}
							style:outline={offline && wallPreferences.cornerRadiusPx === 0
								? '1px solid var(--color-live)'
								: undefined}
							style:outline-offset={wallPreferences.cornerRadiusPx === 0 ? '-1px' : undefined}
						>
							<svg
								viewBox="0 0 320 180"
								preserveAspectRatio={wallPreferences.mediaFit === 'cover'
									? 'xMidYMid slice'
									: 'xMidYMid meet'}
								class="size-full"
								aria-hidden="true"
							>
								<rect width="320" height="180" fill="#0b1017" />
								<rect
									x="0"
									y="0"
									width="320"
									height="100"
									fill={accent}
									opacity={offline ? 0.25 : 0.72}
								/>
								<polygon
									points="90,100 230,100 290,180 30,180"
									fill="#182230"
									opacity={offline ? 0.35 : 0.9}
								/>
								<line x1="160" y1="100" x2="160" y2="180" stroke="#304358" stroke-width="2" />
								<rect
									x="24"
									y="42"
									width="76"
									height="58"
									fill="#131c27"
									stroke="#384c63"
									stroke-width="1.5"
								/>
								<rect
									x="220"
									y="54"
									width="76"
									height="46"
									fill="#131c27"
									stroke="#384c63"
									stroke-width="1.5"
								/>
							</svg>

							<div
								class="pointer-events-none absolute inset-x-0 top-0 flex items-center justify-between gap-2 p-2"
							>
								<span
									class="inline-flex items-center gap-1.5 rounded-sm bg-video/80 px-2 py-1 text-xs font-medium text-white"
								>
									<span
										class="size-1.5 rounded-full {offline
											? 'bg-live'
											: overBudget
												? 'bg-activity'
												: 'bg-healthy'}"
									></span>
									{cameraLabel(item.cameraId)}
								</span>
								{#if item.pinned}
									<span
										class="rounded-sm bg-primary/90 px-1.5 py-0.5 font-mono text-[10px] font-semibold text-on-primary"
									>
										PINNED
									</span>
								{/if}
							</div>

							<div
								class="pointer-events-none absolute inset-x-0 bottom-0 flex items-center justify-between gap-2 p-2 font-mono text-[10px] text-white/80"
							>
								<div class="flex items-center gap-1.5">
									{#if wallPreferences.mediaFit === 'cover'}
										<span class="rounded-xs bg-black/80 px-1.5 py-0.5 text-white">Cropped</span>
									{/if}
									{#if overBudget}
										<span class="rounded-xs bg-activity/85 px-1.5 py-0.5 font-semibold text-black">
											OVER BUDGET
										</span>
									{:else if offline}
										<span class="rounded-xs bg-live/85 px-1.5 py-0.5 text-white">OFFLINE</span>
									{/if}
								</div>
								<span class="rounded-xs bg-black/70 px-1.5 py-0.5">
									{item.columnSpan}×{item.rowSpan}
								</span>
							</div>
						</article>
					{/each}
				</div>
			</div>
		</div>

		{#if accessDialogOpen}
			<PeekDashboardAccessDialog
				dashboard={savedLayout}
				credentials={userCredentials}
				busy={false}
				onsave={handleSaveAudience}
				onclose={() => (accessDialogOpen = false)}
			/>
		{/if}
	{/if}
</main>
