<script lang="ts">
	import { onMount } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import type { CameraSettings } from '$lib/types';
	import {
		maxRuntimeProfiles,
		profileRecordingModes,
		profileOverrideIsEmpty,
		type CameraProfileOverride,
		type RuntimeProfileRegistry,
		type RuntimeProfileStatus
	} from '$lib/runtime-profiles';

	type Props = {
		cameras: readonly CameraSettings[];
		administrator: boolean;
	};

	let { cameras, administrator }: Props = $props();
	const controlClient = useControlClient();
	let registry = $state.raw<RuntimeProfileRegistry | null>(null);
	let draft = $state.raw<RuntimeProfileRegistry | null>(null);
	let activeProfileId = $state<string | null>(null);
	let activeRevision = $state('0');
	let runtimeStatus = $state.raw<RuntimeProfileStatus | null>(null);
	let selectedProfileId = $state('');
	let newProfileName = $state('');
	let loading = $state(true);
	let saving = $state(false);
	let activating = $state(false);
	let error = $state<string | null>(null);
	let announcement = $state<string | null>(null);
	let dirty = $state(false);

	const orderedCameras = $derived(
		[...cameras].sort((left, right) => left.id.localeCompare(right.id))
	);
	const selectedProfile = $derived(draft?.profiles[selectedProfileId] ?? null);
	const configuredCameraIds = $derived(new Set(orderedCameras.map((camera) => camera.id)));
	const missingProfileTargets = $derived.by(() =>
		selectedProfile
			? Object.keys(selectedProfile.cameras)
					.filter((cameraId) => !configuredCameraIds.has(cameraId))
					.sort()
			: []
	);

	onMount(() => {
		let registryWatch: { close: () => Promise<void> } | undefined;
		let activeWatch: { close: () => Promise<void> } | undefined;
		let statusWatch: { close: () => Promise<void> } | undefined;
		let disposed = false;
		void load();
		void controlClient.runtimeProfiles
			.watchStatus((next) => {
				runtimeStatus = next;
			})
			.then((watch) => {
				if (disposed) void watch.close();
				else statusWatch = watch;
			})
			.catch((cause: unknown) => setError(cause));
		void controlClient.runtimeProfiles
			.watchActive((active) => {
				activeProfileId = active.profileId;
				activeRevision = active.revision;
			})
			.then((watch) => {
				if (disposed) void watch.close();
				else activeWatch = watch;
			})
			.catch((cause: unknown) => setError(cause));
		if (administrator) {
			void controlClient.runtimeProfiles
				.watchRegistry((next) => {
					registry = next;
					if (!dirty) draft = structuredClone(next);
				})
				.then((watch) => {
					if (disposed) void watch.close();
					else registryWatch = watch;
				})
				.catch((cause: unknown) => setError(cause));
		}
		return () => {
			disposed = true;
			if (registryWatch) void registryWatch.close();
			if (activeWatch) void activeWatch.close();
			if (statusWatch) void statusWatch.close();
		};
	});

	async function load(): Promise<void> {
		loading = true;
		error = null;
		try {
			const active = await controlClient.runtimeProfiles.getActive();
			activeProfileId = active.profileId;
			activeRevision = active.revision;
			runtimeStatus = await controlClient.runtimeProfiles.getStatus();
			if (administrator) {
				const next = await controlClient.runtimeProfiles.getRegistry();
				registry = next;
				draft = structuredClone(next);
				selectedProfileId = next.active_profile_id ?? Object.keys(next.profiles)[0] ?? '';
			}
		} catch (cause) {
			setError(cause);
		} finally {
			loading = false;
		}
	}

	function setError(cause: unknown): void {
		error = cause instanceof Error ? cause.message : 'Runtime profile state is unavailable.';
	}

	function updateProfileName(name: string): void {
		if (!draft || !selectedProfile) return;
		draft = {
			...draft,
			profiles: {
				...draft.profiles,
				[selectedProfile.id]: { ...selectedProfile, name }
			}
		};
		dirty = true;
	}

	function updateOverride(
		cameraId: string,
		field: keyof CameraProfileOverride,
		value: string
	): void {
		if (!draft || !selectedProfile) return;
		const cameras = { ...selectedProfile.cameras };
		const override: CameraProfileOverride = { ...cameras[cameraId] };
		if (value === '') delete override[field];
		else if (
			field === 'availability' ||
			field === 'privacy_enforced' ||
			field === 'notifications_suppressed'
		) {
			override[field] = value === 'true';
		} else if (
			field === 'recording_mode' &&
			profileRecordingModes.includes(value as (typeof profileRecordingModes)[number])
		) {
			override.recording_mode = value as (typeof profileRecordingModes)[number];
		}
		if (profileOverrideIsEmpty(override)) delete cameras[cameraId];
		else cameras[cameraId] = override;
		draft = {
			...draft,
			profiles: {
				...draft.profiles,
				[selectedProfile.id]: { ...selectedProfile, cameras }
			}
		};
		dirty = true;
	}

	function removeMissingTarget(cameraId: string): void {
		if (!draft || !selectedProfile || !missingProfileTargets.includes(cameraId)) return;
		const cameras = { ...selectedProfile.cameras };
		delete cameras[cameraId];
		draft = {
			...draft,
			profiles: {
				...draft.profiles,
				[selectedProfile.id]: { ...selectedProfile, cameras }
			}
		};
		dirty = true;
	}

	function createProfile(): void {
		if (
			!draft ||
			newProfileName.trim().length === 0 ||
			Object.keys(draft.profiles).length >= maxRuntimeProfiles
		)
			return;
		const name = newProfileName.trim();
		const stem =
			name
				.normalize('NFKD')
				.toLowerCase()
				.replace(/[^a-z0-9]+/g, '-')
				.replace(/^-|-$/g, '') || 'profile';
		let id = stem;
		let suffix = 2;
		while (draft.profiles[id]) id = `${stem}-${suffix++}`;
		const profile = { id, name, revision: '0', cameras: {} };
		draft = { ...draft, profiles: { ...draft.profiles, [id]: profile } };
		selectedProfileId = id;
		newProfileName = '';
		dirty = true;
	}

	async function saveProfiles(): Promise<void> {
		if (!draft || !registry) return;
		saving = true;
		error = null;
		announcement = null;
		try {
			const saved = await controlClient.runtimeProfiles.saveRegistry({
				...draft,
				revision: registry.revision
			});
			registry = saved;
			draft = structuredClone(saved);
			dirty = false;
			announcement = 'Runtime profile changes saved.';
		} catch (cause) {
			setError(cause);
			await load();
		} finally {
			saving = false;
		}
	}

	async function activate(profileId: string | null): Promise<void> {
		activating = true;
		error = null;
		announcement = null;
		try {
			await controlClient.runtimeProfiles.activate(profileId, activeRevision);
			announcement =
				profileId === null
					? 'Base policy selected.'
					: `Activated ${registry?.profiles[profileId]?.name ?? profileId}.`;
		} catch (cause) {
			setError(cause);
			await load();
		} finally {
			activating = false;
		}
	}

	function deleteSelectedProfile(): void {
		if (!draft || !selectedProfile || selectedProfile.id === activeProfileId) return;
		const profiles = { ...draft.profiles };
		delete profiles[selectedProfile.id];
		draft = { ...draft, profiles };
		selectedProfileId = Object.keys(profiles)[0] ?? '';
		dirty = true;
	}
</script>

<section
	id="runtime-profiles"
	class="overflow-hidden rounded-md border border-hairline bg-surface"
	aria-labelledby="runtime-profiles-heading"
>
	<header class="border-b border-hairline px-5 py-5">
		<p class="font-mono text-2xs tracking-caps text-primary-soft">CAMERA POLICY</p>
		<h2 id="runtime-profiles-heading" class="mt-1 text-[28px] leading-[34px] font-semibold">
			Runtime profiles
		</h2>
		<p class="mt-1 max-w-3xl text-sm leading-[22px] text-text-muted">
			Choose Base or one named profile. Sparse settings inherit Base; configured privacy and
			disabled recording remain in force.
		</p>
	</header>

	<div class="space-y-5 p-5">
		{#if loading}
			<p role="status" aria-busy="true" class="text-sm text-text-muted">
				Loading runtime profiles…
			</p>
		{:else}
			<div class="flex flex-wrap items-center gap-3" aria-label="Active runtime profile">
				<p class="text-sm">
					Active policy: <strong
						>{activeProfileId
							? (registry?.profiles[activeProfileId]?.name ?? activeProfileId)
							: 'Base'}</strong
					>
					<span class="text-text-muted"> · revision {activeRevision}</span>
				</p>
				{#if administrator}
					<button
						type="button"
						class="min-h-11 rounded-sm border border-hairline-strong bg-raised px-3 text-sm focus-visible:outline-2 focus-visible:outline-primary"
						disabled={activating || activeProfileId === null || dirty}
						onclick={() => void activate(null)}
					>
						Select Base
					</button>
				{/if}
			</div>

			{#if runtimeStatus}
				<div role="region" class="space-y-2" aria-label="Runtime profile convergence">
					<p class="text-sm font-medium">
						Camera application status for revision {runtimeStatus.revision}
					</p>
					<ul class="divide-y divide-hairline rounded-sm border border-hairline">
						{#each Object.values(runtimeStatus.cameras).sort( (left, right) => left.camera_id.localeCompare(right.camera_id) ) as cameraStatus (cameraStatus.camera_id)}
							<li class="flex flex-wrap items-center justify-between gap-2 p-3 text-sm">
								<span class="min-w-0 truncate font-mono">{cameraStatus.camera_id}</span>
								<span
									class="rounded-sm border border-hairline px-2 py-1 capitalize"
									aria-label="{cameraStatus.state} policy application">{cameraStatus.state}</span
								>
								<span class="text-text-muted"
									>{cameraStatus.policy.available ? 'Available' : 'Unavailable'} ({cameraStatus
										.policy.available_source}) · recording {cameraStatus.policy.recording_mode} ({cameraStatus
										.policy.recording_mode_source}) · privacy {cameraStatus.policy.privacy_enforced
										? 'enforced'
										: 'off'} ({cameraStatus.policy.privacy_source}) · notifications {cameraStatus
										.policy.notifications_enabled
										? 'enabled'
										: 'suppressed'} ({cameraStatus.policy.notifications_source})</span
								>
								{#if cameraStatus.reason}<span class="w-full text-destructive"
										>{cameraStatus.reason}</span
									>{/if}
							</li>
						{/each}
					</ul>
				</div>
			{/if}

			{#if administrator && draft}
				<div class="flex flex-col gap-3 sm:flex-row sm:items-end">
					<label class="grid min-w-0 flex-1 gap-1 text-sm" for="runtime-profile-new-name">
						<span>Create a profile</span>
						<input
							id="runtime-profile-new-name"
							class="min-h-11 rounded-sm border border-hairline-strong bg-background px-3"
							bind:value={newProfileName}
							maxlength="80"
							placeholder="Away"
						/>
					</label>
					<button
						type="button"
						class="min-h-11 rounded-sm bg-primary px-4 text-sm font-medium text-primary-foreground disabled:opacity-50"
						disabled={Object.keys(draft.profiles).length >= maxRuntimeProfiles ||
							newProfileName.trim().length === 0}
						onclick={createProfile}
					>
						Create profile
					</button>
				</div>

				{#if Object.keys(draft.profiles).length > 0}
					<div class="grid gap-5 lg:grid-cols-[minmax(12rem,0.3fr)_minmax(0,1fr)]">
						<div class="space-y-3">
							<label class="grid gap-1 text-sm" for="runtime-profile-selected">
								<span>Edit profile</span>
								<select
									id="runtime-profile-selected"
									class="min-h-11 rounded-sm border border-hairline-strong bg-background px-3"
									bind:value={selectedProfileId}
								>
									{#each Object.values(draft.profiles) as profile (profile.id)}
										<option value={profile.id}>{profile.name}</option>
									{/each}
								</select>
							</label>
							{#if selectedProfile}
								<label class="grid gap-1 text-sm" for="runtime-profile-name">
									<span>Display name</span>
									<input
										id="runtime-profile-name"
										class="min-h-11 rounded-sm border border-hairline-strong bg-background px-3"
										value={selectedProfile.name}
										maxlength="80"
										oninput={(event) => updateProfileName(event.currentTarget.value)}
									/>
								</label>
								<p class="text-xs text-text-muted">Stable profile ID: {selectedProfile.id}</p>
								<div class="flex flex-wrap gap-2">
									<button
										type="button"
										class="min-h-11 rounded-sm bg-primary px-4 text-sm font-medium text-primary-foreground disabled:opacity-50"
										disabled={activating ||
											selectedProfile.id === activeProfileId ||
											dirty ||
											saving}
										onclick={() => void activate(selectedProfile.id)}
									>
										Activate
									</button>
									<button
										type="button"
										class="min-h-11 rounded-sm border border-destructive px-3 text-sm text-destructive disabled:opacity-50"
										disabled={selectedProfile.id === activeProfileId || saving}
										onclick={deleteSelectedProfile}
									>
										Delete
									</button>
								</div>
							{/if}
						</div>

						{#if selectedProfile}
							<div class="space-y-3">
								<h3 class="text-sm font-semibold">Camera overrides</h3>
								{#if missingProfileTargets.length > 0}
									<div
										role="region"
										aria-label="Unavailable profile targets"
										class="border-warning space-y-2 rounded-sm border px-3 py-3"
									>
										<p class="text-sm text-text-muted">
											These targets no longer match a configured camera. Their stored overrides are
											not applied. Remove them from this profile to clear the stale settings.
										</p>
										<ul class="divide-y divide-hairline rounded-sm border border-hairline">
											{#each missingProfileTargets as cameraId (cameraId)}
												<li class="flex min-w-0 flex-wrap items-center justify-between gap-2 p-2">
													<span class="min-w-0 truncate font-mono text-sm">{cameraId}</span>
													<button
														type="button"
														class="min-h-11 rounded-sm border border-destructive px-3 text-sm text-destructive focus-visible:outline-2 focus-visible:outline-primary"
														aria-label="Remove unavailable target {cameraId}"
														onclick={() => removeMissingTarget(cameraId)}
													>
														Remove
													</button>
												</li>
											{/each}
										</ul>
									</div>
								{/if}
								{#if orderedCameras.length === 0}
									<p class="text-sm text-text-muted">
										Add a camera before assigning profile overrides.
									</p>
								{:else}
									<ul class="divide-y divide-hairline rounded-sm border border-hairline">
										{#each orderedCameras as camera (camera.id)}
											{@const override = selectedProfile.cameras[camera.id] ?? {}}
											<li
												class="grid gap-3 p-3 md:grid-cols-[minmax(9rem,1fr)_repeat(4,minmax(8rem,1fr))]"
											>
												<div class="min-w-0 self-center">
													<p class="truncate text-sm font-medium">
														{camera.display_name || camera.id}
													</p>
													<p class="truncate font-mono text-2xs text-text-muted">{camera.id}</p>
												</div>
												<label
													class="grid gap-1 text-xs"
													for="availability-{selectedProfile.id}-{camera.id}"
												>
													<span>Availability</span>
													<select
														id="availability-{selectedProfile.id}-{camera.id}"
														class="min-h-10 rounded-sm border border-hairline bg-background px-2"
														value={override.availability === undefined
															? ''
															: String(override.availability)}
														onchange={(event) =>
															updateOverride(camera.id, 'availability', event.currentTarget.value)}
													>
														<option value="">Base</option><option value="true">Available</option
														><option value="false">Unavailable</option>
													</select>
												</label>
												<label
													class="grid gap-1 text-xs"
													for="recording-{selectedProfile.id}-{camera.id}"
												>
													<span>Recording</span>
													<select
														id="recording-{selectedProfile.id}-{camera.id}"
														class="min-h-10 rounded-sm border border-hairline bg-background px-2"
														value={override.recording_mode ?? ''}
														onchange={(event) =>
															updateOverride(
																camera.id,
																'recording_mode',
																event.currentTarget.value
															)}
													>
														<option value="">Base</option
														>{#each profileRecordingModes as mode}<option value={mode}
																>{mode}</option
															>{/each}
													</select>
												</label>
												<label
													class="grid gap-1 text-xs"
													for="privacy-{selectedProfile.id}-{camera.id}"
												>
													<span>Privacy</span>
													<select
														id="privacy-{selectedProfile.id}-{camera.id}"
														class="min-h-10 rounded-sm border border-hairline bg-background px-2"
														value={override.privacy_enforced === undefined
															? ''
															: String(override.privacy_enforced)}
														onchange={(event) =>
															updateOverride(
																camera.id,
																'privacy_enforced',
																event.currentTarget.value
															)}
													>
														<option value="">Base</option><option value="true">Enforce</option
														><option value="false">Allow when Base allows</option>
													</select>
												</label>
												<label
													class="grid gap-1 text-xs"
													for="notifications-{selectedProfile.id}-{camera.id}"
												>
													<span>Notifications</span>
													<select
														id="notifications-{selectedProfile.id}-{camera.id}"
														class="min-h-10 rounded-sm border border-hairline bg-background px-2"
														value={override.notifications_suppressed === undefined
															? ''
															: String(override.notifications_suppressed)}
														onchange={(event) =>
															updateOverride(
																camera.id,
																'notifications_suppressed',
																event.currentTarget.value === ''
																	? ''
																	: String(event.currentTarget.value === 'true')
															)}
													>
														<option value="">Base</option><option value="true">Suppress</option
														><option value="false">Allow when Base allows</option>
													</select>
												</label>
											</li>
										{/each}
									</ul>
								{/if}
							</div>
						{/if}
					</div>
				{/if}

				<div class="flex flex-wrap items-center gap-3 border-t border-hairline pt-4">
					<button
						type="button"
						class="min-h-11 rounded-sm bg-primary px-4 text-sm font-medium text-primary-foreground disabled:opacity-50"
						disabled={!dirty || saving}
						onclick={() => void saveProfiles()}
					>
						{saving ? 'Saving…' : 'Save profile changes'}
					</button>
					{#if dirty}<span class="text-sm text-text-muted">Unsaved changes</span>{/if}
				</div>
			{/if}
		{/if}

		{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
		{#if announcement}<p role="status" aria-live="polite" class="text-sm text-text-muted">
				{announcement}
			</p>{/if}
	</div>
</section>
