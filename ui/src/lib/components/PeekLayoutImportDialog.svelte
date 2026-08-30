<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import type { CameraListItem } from '$lib/types';
	import type { PeekLayoutRegistry } from '$lib/peek-layout';
	import { applyPeekLayoutImport, type PeekLayoutImportPreview } from '$lib/peek-layout-exchange';
	import AlertTriangleIcon from '@lucide/svelte/icons/triangle-alert';
	import XIcon from '@lucide/svelte/icons/x';

	type Props = {
		current: PeekLayoutRegistry;
		preview: PeekLayoutImportPreview;
		cameras: readonly CameraListItem[];
		busy: boolean;
		onapply: (registry: PeekLayoutRegistry) => Promise<boolean>;
		onclose: () => void;
	};

	let { current, preview, cameras, busy, onapply, onclose }: Props = $props();
	let closeButton: HTMLButtonElement | null = $state(null);
	let mappings = $state.raw<Record<string, string | null | undefined>>(
		Object.fromEntries(
			untrack(() => preview.missingCameraIds.map((cameraId) => [cameraId, undefined]))
		)
	);
	let conflictResolution = $state<'duplicate' | 'reject' | 'replace'>('reject');
	let error = $state<string | null>(null);
	let mappingsComplete = $derived(
		preview.missingCameraIds.every((cameraId) => mappings[cameraId] !== undefined)
	);
	let conflictsResolved = $derived(
		preview.conflictingLayoutIds.length === 0 || conflictResolution !== 'reject'
	);
	let canApply = $derived(
		!busy && preview.unsupportedFields.length === 0 && mappingsComplete && conflictsResolved
	);

	onMount(() => closeButton?.focus());

	function cameraLabel(camera: CameraListItem): string {
		return camera.name ? `${camera.name} (${camera.id})` : camera.id;
	}

	function updateMapping(cameraId: string, event: Event): void {
		if (!(event.currentTarget instanceof HTMLSelectElement)) return;
		const value = event.currentTarget.value;
		mappings = {
			...mappings,
			[cameraId]: value === '' ? undefined : value === '__omit__' ? null : value
		};
		error = null;
	}

	async function applyImport(): Promise<void> {
		if (!canApply) return;
		try {
			const candidate = applyPeekLayoutImport(current, preview, {
				ownerId: 'server',
				targetScope: 'shared',
				targetAudience: { everyone: false, credentialIds: [] },
				availableCameraIds: cameras.map((camera) => camera.id),
				missingCameraMappings: Object.fromEntries(
					Object.entries(mappings).filter(
						(entry): entry is [string, string | null] => entry[1] !== undefined
					)
				),
				conflictResolution
			});
			if (await onapply(candidate)) {
				onclose();
			} else {
				error = 'The imported layouts could not be saved. The preview was preserved.';
			}
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Layout import failed.';
		}
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || busy) return;
		event.preventDefault();
		onclose();
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="fixed inset-0 z-[70] grid place-items-center bg-black/55 p-4" role="presentation">
	<dialog
		open
		class="m-0 max-h-[min(44rem,calc(100vh-2rem))] w-full max-w-2xl overflow-y-auto rounded-md border border-hairline-strong bg-surface p-0 text-foreground shadow-xl"
		aria-modal="true"
		aria-labelledby="peek-layout-import-title"
	>
		<header class="flex min-h-14 items-center gap-3 border-b border-hairline px-4">
			<div class="min-w-0 flex-1">
				<h2 id="peek-layout-import-title" class="text-sm font-semibold">Import dashboards</h2>
				<p class="text-xs text-text-muted">
					{preview.layouts.length}
					{preview.layouts.length === 1 ? 'dashboard' : 'dashboards'} ·
					{preview.layouts.reduce((count, layout) => count + layout.items.length, 0)} camera
					{preview.layouts.reduce((count, layout) => count + layout.items.length, 0) === 1
						? ''
						: 's'}
				</p>
			</div>
			<button
				bind:this={closeButton}
				type="button"
				class="grid size-8 place-items-center rounded-sm text-text-muted hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				aria-label="Close layout import"
				disabled={busy}
				onclick={onclose}
			>
				<XIcon class="size-4" />
			</button>
		</header>

		<div class="space-y-5 p-4">
			<div
				class="max-h-48 divide-y divide-hairline overflow-y-auto border-y border-hairline"
				aria-label="Imported dashboards"
			>
				{#each preview.layouts as layout (layout.id)}
					<div class="space-y-1 py-2.5 text-xs">
						<div class="flex min-w-0 items-center gap-2">
							<span class="truncate font-semibold">{layout.name}</span>
							<span class="ml-auto shrink-0 font-mono text-2xs text-text-faint">
								{layout.scope.toUpperCase()} · {layout.items.length}
							</span>
						</div>
						<p class="font-mono text-2xs break-all text-text-muted">
							{layout.items.map((item) => item.cameraId).join(' · ') || 'No cameras'}
						</p>
						<p class="text-2xs text-text-faint">Imports with Administrator-only access</p>
					</div>
				{/each}
			</div>

			{#if preview.unsupportedFields.length > 0}
				<div
					class="flex gap-3 border-y border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs text-destructive"
					role="alert"
				>
					<AlertTriangleIcon class="mt-0.5 size-4 shrink-0" />
					<div>
						<p class="font-semibold">Unsupported fields</p>
						<p class="mt-1 font-mono break-all">{preview.unsupportedFields.join(', ')}</p>
					</div>
				</div>
			{/if}

			{#if preview.conflictingLayoutIds.length > 0}
				<label class="block space-y-1.5 text-xs font-medium">
					<span
						>{preview.conflictingLayoutIds.length} conflicting layout ID{preview
							.conflictingLayoutIds.length === 1
							? ''
							: 's'}</span
					>
					<select
						class="h-9 w-full rounded-sm border border-hairline-strong bg-raised px-2 text-xs focus:border-ring focus:ring-1 focus:ring-ring focus:outline-none"
						bind:value={conflictResolution}
					>
						<option value="reject">Choose a resolution</option>
						<option value="duplicate">Import as new layouts</option>
						<option value="replace">Replace matching layouts</option>
					</select>
				</label>
			{/if}

			{#if preview.missingCameraIds.length > 0}
				<div class="space-y-3">
					<h3 class="text-xs font-semibold">Missing cameras</h3>
					{#each preview.missingCameraIds as cameraId (cameraId)}
						<label
							class="grid gap-1.5 text-xs sm:grid-cols-[minmax(0,1fr)_minmax(12rem,1fr)] sm:items-center"
						>
							<span class="truncate font-mono text-text-muted" title={cameraId}>{cameraId}</span>
							<select
								class="h-9 min-w-0 rounded-sm border border-hairline-strong bg-raised px-2 text-xs focus:border-ring focus:ring-1 focus:ring-ring focus:outline-none"
								value={mappings[cameraId] === null ? '__omit__' : (mappings[cameraId] ?? '')}
								onchange={(event) => updateMapping(cameraId, event)}
							>
								<option value="">Choose camera</option>
								<option value="__omit__">Omit this camera</option>
								{#each cameras as camera (camera.id)}
									<option value={camera.id}>{cameraLabel(camera)}</option>
								{/each}
							</select>
						</label>
					{/each}
				</div>
			{/if}

			{#if error}
				<p class="text-xs text-destructive" role="alert">{error}</p>
			{/if}
		</div>

		<footer class="flex justify-end gap-2 border-t border-hairline px-4 py-3">
			<button
				type="button"
				class="h-8 rounded-sm border border-hairline bg-raised px-3 text-xs font-medium hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				disabled={busy}
				onclick={onclose}>Cancel</button
			>
			<button
				type="button"
				class="h-8 rounded-sm bg-primary px-3 text-xs font-semibold text-primary-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
				disabled={!canApply}
				onclick={applyImport}
			>
				{busy ? 'Importing…' : 'Import'}
			</button>
		</footer>
	</dialog>
</div>
