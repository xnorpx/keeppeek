<script lang="ts">
	import { onMount } from 'svelte';
	import { Portal } from 'bits-ui';
	import type { AccessCredential, CameraAccessSettings } from '$lib/access';
	import type { ControlClient } from '$lib/control-client';
	import { cameraAccessCapability } from '$lib/control-client-camera-access';
	import { Button } from '$lib/components/ui/button/index.js';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SaveIcon from '@lucide/svelte/icons/save';
	import XIcon from '@lucide/svelte/icons/x';

	type Props = {
		credential: Pick<AccessCredential, 'id' | 'name'>;
		controller: Pick<
			ControlClient,
			'getCameras' | 'getCameraAccess' | 'saveCameraAccess' | 'onCapabilities'
		>;
		onclose: () => void;
		onsaved: () => void;
	};
	let { credential, controller, onclose, onsaved }: Props = $props();
	let dialog = $state<HTMLDialogElement | null>(null);
	let draft = $state.raw<CameraAccessSettings | null>(null);
	let cameras = $state.raw<{ id: string; name: string | null }[]>([]);
	let loading = $state(true);
	let saving = $state(false);
	let available = $state(false);
	let error = $state<string | null>(null);
	let generation = 0;
	const choices = $derived.by(() => {
		const known = new Set(cameras.map((camera) => camera.id));
		const missing = draft?.cameraIds.filter((id) => !known.has(id)) ?? [];
		return [...cameras, ...missing.map((id) => ({ id, name: `Unavailable camera (${id})` }))];
	});
	const groups = $derived(
		[...new Set([...(draft?.availableGroupIds ?? []), ...(draft?.groupIds ?? [])])].sort()
	);

	$effect(() => {
		const element = dialog;
		if (!element) return;
		element.showModal();
		return () => element.close();
	});

	onMount(() => {
		let started = false;
		const unsubscribe = controller.onCapabilities((ids) => {
			available = ids.includes(cameraAccessCapability);
			if (!available) {
				generation++;
				loading = false;
				saving = false;
			} else if (!started) {
				started = true;
				void load();
			}
		});
		return () => {
			generation++;
			unsubscribe();
		};
	});

	async function load(): Promise<void> {
		const current = ++generation;
		loading = true;
		error = null;
		try {
			const [settings, sources] = await Promise.all([
				controller.getCameraAccess(credential.id),
				controller.getCameras()
			]);
			if (current !== generation) return;
			draft = settings;
			cameras = sources.map((camera) => ({ id: camera.id, name: camera.name ?? null }));
		} catch (cause) {
			if (current === generation)
				error = cause instanceof Error ? cause.message : 'Camera access could not be loaded.';
		} finally {
			if (current === generation) loading = false;
		}
	}

	function toggleCamera(cameraId: string): void {
		if (!draft) return;
		const cameraIds = draft.cameraIds.includes(cameraId)
			? draft.cameraIds.filter((id) => id !== cameraId)
			: [...draft.cameraIds, cameraId];
		draft = { ...draft, cameraIds };
	}

	function toggleGroup(groupId: string): void {
		if (!draft) return;
		const groupIds = draft.groupIds.includes(groupId)
			? draft.groupIds.filter((id) => id !== groupId)
			: [...draft.groupIds, groupId];
		draft = { ...draft, groupIds };
	}

	function selectMode(allCameras: boolean): void {
		if (draft) draft = { ...draft, allCameras, groupIds: [], cameraIds: [] };
	}

	async function save(): Promise<void> {
		if (!draft || !available || saving) return;
		const current = generation;
		saving = true;
		error = null;
		try {
			await controller.saveCameraAccess(draft);
			if (current !== generation) return;
			onsaved();
			onclose();
		} catch (cause) {
			if (current === generation)
				error = cause instanceof Error ? cause.message : 'Camera access could not be saved.';
		} finally {
			if (current === generation) saving = false;
		}
	}
</script>

<Portal>
	<dialog
		bind:this={dialog}
		data-user-access-paper
		class="m-auto max-h-[85dvh] w-[calc(100%-2rem)] max-w-[645px] overflow-auto rounded-md border border-hairline bg-surface p-0 text-foreground shadow-xl backdrop:bg-black/55"
		aria-labelledby="camera-access-title"
		oncancel={(event) => {
			event.preventDefault();
			if (!saving) onclose();
		}}
	>
		<header
			class="flex min-h-14 items-center gap-3 border-b border-hairline px-4 py-4 sm:px-[18px]"
		>
			<div class="min-w-0 flex-1">
				<h2 id="camera-access-title" class="text-lg-plus leading-[22px] font-semibold">
					User access
				</h2>
				<p class="mt-1 text-sm leading-4 break-words text-text-muted">{credential.name}</p>
			</div>
			<Button
				variant="ghost"
				size="icon-sm"
				onclick={onclose}
				disabled={saving}
				aria-label="Close user access"
				title="Close user access"><XIcon class="size-4" /></Button
			>
		</header>
		<div class="space-y-3 p-4 sm:p-[18px]">
			{#if !available}<p role="status" class="text-xs text-text-muted">
					User access unavailable.
				</p>{/if}
			{#if loading}<p role="status" class="text-sm text-text-muted">Loading permissions...</p>{/if}
			{#if draft}
				<fieldset disabled={loading || saving || !available} class="space-y-4 disabled:opacity-60">
					<legend class="sr-only">User access scope</legend>
					<div class="grid grid-cols-[1fr_2fr] gap-2">
						{#each [{ label: 'Everything', all: true }, { label: 'Selected groups and cameras', all: false }] as mode (mode.all)}
							<label
								class="flex min-h-10 items-center gap-2 rounded-[3px] border px-3 py-2 text-xs-plus leading-4"
								class:border-primary={draft.allCameras === mode.all}
								class:border-hairline-strong={draft.allCameras !== mode.all}
							>
								<input
									type="radio"
									name={`user-access-${credential.id}`}
									checked={draft.allCameras === mode.all}
									onchange={() => selectMode(mode.all)}
									class="size-3.5 shrink-0 accent-primary"
								/>
								<span>{mode.label}</span>
							</label>
						{/each}
					</div>
					<div class="grid gap-5 sm:grid-cols-2">
						<section aria-label="Camera groups" class="min-w-0">
							<h3
								class="border-b border-hairline-strong pb-2 font-mono text-xs leading-[14px] text-text-faint"
							>
								GROUPS
							</h3>
							<div class="max-h-64 overflow-auto">
								{#each groups as group (group)}
									<label
										data-user-access-row
										class="flex min-h-[52px] items-center gap-3 border-b border-hairline py-2 text-md leading-[18px] sm:min-h-11"
									>
										<input
											type="checkbox"
											aria-label={group}
											checked={draft.allCameras || draft.groupIds.includes(group)}
											disabled={draft.allCameras}
											onchange={() => toggleGroup(group)}
											class="size-4 shrink-0 accent-primary"
										/>
										<span class="min-w-0 break-words">{group}</span>
									</label>
								{:else}<p class="py-4 text-sm text-text-muted">No camera groups</p>{/each}
							</div>
						</section>
						<section aria-label="Individual cameras" class="min-w-0">
							<h3
								class="border-b border-hairline-strong pb-2 font-mono text-xs leading-[14px] text-text-faint"
							>
								CAMERAS
							</h3>
							<div class="max-h-64 overflow-auto">
								{#each choices as camera (camera.id)}
									<label
										data-user-access-row
										class="flex min-h-[52px] items-center gap-3 border-b border-hairline py-2 text-md leading-[18px] sm:min-h-11"
									>
										<input
											type="checkbox"
											aria-label={camera.name ?? camera.id}
											checked={draft.allCameras || draft.cameraIds.includes(camera.id)}
											disabled={draft.allCameras}
											onchange={() => toggleCamera(camera.id)}
											class="size-4 shrink-0 accent-primary"
										/>
										<span class="min-w-0 break-words">{camera.name ?? camera.id}</span>
									</label>
								{:else}<p class="py-4 text-sm text-text-muted">No cameras configured</p>{/each}
							</div>
						</section>
					</div>
					<p class="text-xs text-text-muted">
						{draft.allCameras
							? 'All groups and cameras'
							: `${draft.groupIds.length} ${draft.groupIds.length === 1 ? 'group' : 'groups'} / ${draft.cameraIds.length} individual ${draft.cameraIds.length === 1 ? 'camera' : 'cameras'}`}
					</p>
				</fieldset>
			{/if}
			{#if error}<p role="alert" class="text-xs break-words text-destructive">{error}</p>{/if}
		</div>
		<footer
			class="flex flex-wrap items-center justify-end gap-2 border-t border-hairline px-4 py-3"
		>
			<Button
				variant="ghost"
				size="icon-sm"
				aria-label="Reload permissions"
				title="Reload permissions"
				disabled={loading || saving || !available}
				onclick={() => void load()}><RefreshCwIcon class="size-4" /></Button
			>
			<Button variant="ghost" size="sm" disabled={saving} onclick={onclose}>Cancel</Button>
			<Button
				size="sm"
				disabled={!draft || loading || saving || !available}
				onclick={() => void save()}
				><SaveIcon class="size-3.5" />{saving ? 'Saving...' : 'Save access'}</Button
			>
		</footer>
	</dialog>
</Portal>
