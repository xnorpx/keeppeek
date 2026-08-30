<script lang="ts">
	import { tick } from 'svelte';
	import type { AccessCredential } from '$lib/access';
	import type { CameraListItem } from '$lib/types';
	import {
		createPeekLayout,
		deletePeekLayout,
		duplicatePeekLayout,
		peekLayoutDraft,
		renamePeekLayout,
		selectPeekLayout,
		updatePeekLayoutAudience,
		type PeekLayout,
		type PeekLayoutAudience,
		type PeekLayoutRegistry
	} from '$lib/peek-layout';
	import {
		exportPeekLayoutRegistry,
		previewPeekLayoutImport,
		type PeekLayoutImportPreview
	} from '$lib/peek-layout-exchange';
	import CopyIcon from '@lucide/svelte/icons/copy';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import UploadIcon from '@lucide/svelte/icons/upload';
	import UsersIcon from '@lucide/svelte/icons/users';
	import XIcon from '@lucide/svelte/icons/x';
	import PeekDashboardAccessDialog from './PeekDashboardAccessDialog.svelte';
	import PeekDashboardAudiencePicker from './PeekDashboardAudiencePicker.svelte';
	import PeekLayoutImportDialog from './PeekLayoutImportDialog.svelte';

	type Props = {
		registry: PeekLayoutRegistry;
		activeLayout: PeekLayout;
		cameras: readonly CameraListItem[];
		credentials: readonly AccessCredential[];
		busy: boolean;
		onrefreshcredentials: () => Promise<void>;
		onchange: (registry: PeekLayoutRegistry) => Promise<boolean>;
	};

	let {
		registry,
		activeLayout,
		cameras,
		credentials,
		busy,
		onrefreshcredentials,
		onchange
	}: Props = $props();
	let nameMode = $state<'new' | 'rename' | null>(null);
	let layoutName = $state('');
	let nameInput: HTMLInputElement | null = $state(null);
	let importInput: HTMLInputElement | null = $state(null);
	let importPreview = $state.raw<PeekLayoutImportPreview | null>(null);
	let deleteArmed = $state(false);
	let newAudience = $state.raw<PeekLayoutAudience>({ everyone: false, credentialIds: [] });
	let accessOpen = $state(false);
	let error = $state<string | null>(null);
	let canModifyActive = $derived(activeLayout.id !== 'default');
	let canDeleteActive = $derived(canModifyActive && registry.layouts.length > 1);

	async function selectLayout(event: Event): Promise<void> {
		if (!(event.currentTarget instanceof HTMLSelectElement)) return;
		deleteArmed = false;
		error = null;
		if (!(await onchange(selectPeekLayout(registry, event.currentTarget.value)))) {
			error = 'The selected layout could not be saved.';
		}
	}

	async function openNameDialog(mode: 'new' | 'rename'): Promise<void> {
		nameMode = mode;
		layoutName = mode === 'rename' ? activeLayout.name : '';
		newAudience = { everyone: false, credentialIds: [] };
		error = null;
		await tick();
		nameInput?.focus();
		nameInput?.select();
	}

	async function applyName(): Promise<void> {
		if (nameMode === null || busy) return;
		try {
			const candidate =
				nameMode === 'new'
					? createPeekLayout(registry, {
							id: crypto.randomUUID(),
							name: layoutName,
							ownerId: 'server',
							scope: 'shared',
							audience: newAudience,
							draft: peekLayoutDraft(activeLayout)
						})
					: renamePeekLayout(registry, activeLayout.id, layoutName);
			if (await onchange(candidate)) {
				nameMode = null;
			} else {
				error = 'The layout could not be saved. The dialog values were preserved.';
			}
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Layout update failed.';
		}
	}

	async function duplicateLayout(): Promise<void> {
		error = null;
		try {
			const saved = await onchange(
				duplicatePeekLayout(registry, activeLayout.id, {
					id: crypto.randomUUID(),
					name: [...`${activeLayout.name} copy`].slice(0, 80).join(''),
					ownerId: 'server'
				})
			);
			if (!saved) error = 'The duplicate layout could not be saved.';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Layout duplication failed.';
		}
	}

	function saveAccess(audience: PeekLayoutAudience): Promise<boolean> {
		return onchange(updatePeekLayoutAudience(registry, activeLayout.id, audience));
	}

	async function openAccess(): Promise<void> {
		await onrefreshcredentials();
		await tick();
		accessOpen = true;
	}

	async function deleteLayout(): Promise<void> {
		if (!deleteArmed) {
			deleteArmed = true;
			return;
		}
		error = null;
		if (await onchange(deletePeekLayout(registry, activeLayout.id))) {
			deleteArmed = false;
		} else {
			error = 'The layout could not be deleted.';
		}
	}

	function download(scope: 'active' | 'all'): void {
		const content = exportPeekLayoutRegistry(
			registry,
			scope === 'active' ? activeLayout.id : undefined
		);
		const url = URL.createObjectURL(new Blob([content], { type: 'application/json' }));
		const anchor = document.createElement('a');
		anchor.href = url;
		anchor.download =
			scope === 'active'
				? `keeppeek-layout-${safeFilename(activeLayout.name)}.json`
				: 'keeppeek-layouts.json';
		anchor.click();
		URL.revokeObjectURL(url);
	}

	async function readImport(event: Event): Promise<void> {
		if (!(event.currentTarget instanceof HTMLInputElement)) return;
		const file = event.currentTarget.files?.[0];
		event.currentTarget.value = '';
		if (!file) return;
		error = null;
		try {
			if (file.size > 256 * 1_024) throw new Error('Layout import is too large.');
			importPreview = previewPeekLayoutImport(
				await file.text(),
				registry,
				cameras.map((camera) => camera.id)
			);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Layout import could not be read.';
		}
	}

	function safeFilename(value: string): string {
		return (
			value
				.trim()
				.toLocaleLowerCase()
				.replaceAll(/[^a-z0-9]+/g, '-')
				.replaceAll(/^-|-$/g, '') || 'layout'
		);
	}

	function handleNameKeydown(event: KeyboardEvent): void {
		if (event.key === 'Enter') {
			event.preventDefault();
			void applyName();
		} else if (event.key === 'Escape') {
			nameMode = null;
		}
	}
</script>

<div data-peek-layout-toolbar class="flex min-w-0 items-center gap-1.5">
	<label class="flex min-w-0 items-center gap-1.5 text-xs text-text-muted">
		<Grid2X2Icon class="size-3.5 shrink-0" />
		<span class="shrink-0 font-medium">Dashboard</span>
		<select
			aria-label="Dashboard to manage"
			class="h-8 max-w-48 min-w-28 rounded-sm border border-hairline bg-raised px-2 text-xs font-medium text-foreground focus:border-ring focus:ring-1 focus:ring-ring focus:outline-none"
			value={activeLayout.id}
			disabled={busy}
			onchange={selectLayout}
		>
			{#each registry.layouts as layout (layout.id)}
				<option value={layout.id}>{layout.name}</option>
			{/each}
		</select>
	</label>
	<div data-peek-layout-actions class="flex min-w-0 items-center gap-1.5">
		<button
			type="button"
			class="grid size-8 place-items-center rounded-sm border border-hairline bg-raised text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			aria-label="New dashboard"
			title="New dashboard"
			disabled={busy || registry.layouts.length >= 32}
			onclick={() => openNameDialog('new')}
		>
			<PlusIcon class="size-3.5" />
		</button>
		<button
			type="button"
			class="grid size-8 place-items-center rounded-sm border border-hairline bg-raised text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40"
			aria-label="Rename dashboard"
			title="Rename dashboard"
			disabled={busy || !canModifyActive}
			onclick={() => openNameDialog('rename')}
		>
			<PencilIcon class="size-3.5" />
		</button>
		<button
			type="button"
			class="grid size-8 place-items-center rounded-sm border border-hairline bg-raised text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40"
			aria-label="Manage access"
			title="Manage access"
			disabled={busy || !canModifyActive}
			onclick={() => void openAccess()}
		>
			<UsersIcon class="size-3.5" />
		</button>
		<button
			type="button"
			class="grid size-8 place-items-center rounded-sm border border-hairline bg-raised text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40"
			aria-label="Duplicate dashboard"
			title="Duplicate dashboard"
			disabled={busy || registry.layouts.length >= 32}
			onclick={duplicateLayout}
		>
			<CopyIcon class="size-3.5" />
		</button>
		<button
			type="button"
			class="grid size-8 place-items-center rounded-sm border border-hairline bg-raised text-text-muted hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-40"
			aria-label={deleteArmed ? 'Confirm delete dashboard' : 'Delete dashboard'}
			title={deleteArmed ? 'Confirm delete dashboard' : 'Delete dashboard'}
			disabled={busy || !canDeleteActive}
			onclick={deleteLayout}
		>
			<Trash2Icon class="size-3.5" />
		</button>
		<details class="relative">
			<summary
				aria-label="Export dashboards"
				class="flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-sm border border-hairline bg-raised px-2 text-xs font-medium text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			>
				<DownloadIcon class="size-3.5" /><span data-peek-action-label>Export</span>
			</summary>
			<div
				class="absolute top-9 right-0 z-40 w-40 rounded-sm border border-hairline-strong bg-surface p-1 shadow-lg"
			>
				<button
					type="button"
					class="h-8 w-full rounded-xs px-2 text-left text-xs hover:bg-muted"
					onclick={() => download('active')}>Current dashboard</button
				>
				<button
					type="button"
					class="h-8 w-full rounded-xs px-2 text-left text-xs hover:bg-muted"
					onclick={() => download('all')}>All dashboards</button
				>
			</div>
		</details>
		<button
			type="button"
			aria-label="Import dashboards"
			class="inline-flex h-8 items-center gap-1.5 rounded-sm border border-hairline bg-raised px-2 text-xs font-medium text-text-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
			disabled={busy}
			onclick={() => importInput?.click()}
		>
			<UploadIcon class="size-3.5" /><span data-peek-action-label>Import</span>
		</button>
		<input
			bind:this={importInput}
			type="file"
			class="sr-only"
			accept="application/json,.json"
			aria-label="Choose dashboard import file"
			onchange={readImport}
		/>
	</div>
</div>

{#if error}
	<p class="mt-1 text-right text-xs text-destructive" role="alert">{error}</p>
{/if}

{#if nameMode}
	<div class="fixed inset-0 z-[70] grid place-items-center bg-black/55 p-4" role="presentation">
		<dialog
			open
			class="m-0 w-full max-w-sm rounded-md border border-hairline-strong bg-surface p-0 text-foreground shadow-xl"
			aria-modal="true"
			aria-labelledby="peek-layout-name-title"
		>
			<header class="flex min-h-14 items-center gap-3 border-b border-hairline px-4">
				<h2 id="peek-layout-name-title" class="flex-1 text-sm font-semibold">
					{nameMode === 'new' ? 'New dashboard' : 'Rename dashboard'}
				</h2>
				<button
					type="button"
					class="grid size-8 place-items-center rounded-sm text-text-muted hover:bg-muted"
					aria-label="Close layout name dialog"
					onclick={() => (nameMode = null)}><XIcon class="size-4" /></button
				>
			</header>
			<div class="space-y-2 p-4">
				<label for="peek-layout-name" class="text-xs font-medium">Dashboard name</label>
				<input
					bind:this={nameInput}
					id="peek-layout-name"
					type="text"
					maxlength="80"
					class="h-9 w-full rounded-sm border border-hairline-strong bg-raised px-3 text-sm focus:border-ring focus:ring-1 focus:ring-ring focus:outline-none"
					bind:value={layoutName}
					onkeydown={handleNameKeydown}
				/>
				{#if nameMode === 'new'}
					<div class="pt-2">
						<PeekDashboardAudiencePicker
							{credentials}
							audience={newAudience}
							onchange={(audience) => (newAudience = audience)}
						/>
					</div>
				{/if}
			</div>
			<footer class="flex justify-end gap-2 border-t border-hairline px-4 py-3">
				<button
					type="button"
					class="h-8 rounded-sm border border-hairline bg-raised px-3 text-xs"
					onclick={() => (nameMode = null)}>Cancel</button
				>
				<button
					type="button"
					class="h-8 rounded-sm bg-primary px-3 text-xs font-semibold text-primary-foreground disabled:opacity-50"
					disabled={busy || layoutName.trim().length === 0}
					onclick={applyName}>{busy ? 'Saving…' : 'Save'}</button
				>
			</footer>
		</dialog>
	</div>
{/if}

{#if accessOpen}
	<PeekDashboardAccessDialog
		dashboard={activeLayout}
		{credentials}
		{busy}
		onsave={saveAccess}
		onclose={() => (accessOpen = false)}
	/>
{/if}

{#if importPreview}
	<PeekLayoutImportDialog
		current={registry}
		preview={importPreview}
		{cameras}
		{busy}
		onapply={onchange}
		onclose={() => (importPreview = null)}
	/>
{/if}
