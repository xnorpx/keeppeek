<script lang="ts">
	import { beforeNavigate } from '$app/navigation';
	import { onMount, tick, untrack } from 'svelte';
	import { useCapabilityState } from '$lib/capability-context';
	import { ConfigurationRequestError } from '$lib/configuration-error';
	import {
		cameraPolicyPatch,
		defaultPolicyPatch,
		emptyPolicyPatchDraft,
		policyPatchDraftDirty,
		type PolicyPatchDraft
	} from '$lib/configuration-editor';
	import { useControlClient } from '$lib/control-context';
	import ConfigurationPolicyFields from '$lib/components/ConfigurationPolicyFields.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import type {
		CameraBackend,
		CameraRecordingMode,
		CameraTransport,
		ConfigurationApplyResult,
		ConfigurationPlan,
		ConfigurationSnapshot,
		ConfigurationTargetSelector,
		ConfigurationTemplate,
		ConfigurationTemplateImportPreview,
		ConfigurationTemplateValues
	} from '$lib/types';
	import AlertTriangleIcon from '@lucide/svelte/icons/triangle-alert';
	import CheckIcon from '@lucide/svelte/icons/check';
	import CopyIcon from '@lucide/svelte/icons/copy';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import FileUpIcon from '@lucide/svelte/icons/file-up';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SearchIcon from '@lucide/svelte/icons/search';
	import TrashIcon from '@lucide/svelte/icons/trash-2';
	import XIcon from '@lucide/svelte/icons/x';

	type Tab = 'overview' | 'defaults' | 'templates' | 'bulk';
	type BulkTargetMode = 'selected' | 'filtered' | 'group' | 'all';
	type BulkChangeMode = 'patch' | 'template';
	type TemplateDraft = {
		template_id: string;
		version: number;
		name: string;
		description: string;
		username_secret_reference: string;
		password_secret_reference: string;
		onvif_port: string;
		http_port: string;
		backend: '' | CameraBackend;
		transport: '' | CameraTransport;
		record_generic_motion_events: '' | 'true' | 'false';
		recording_mode: '' | CameraRecordingMode;
		event_recording_duration_secs: string;
	};
	type Props = {
		selectedCameraIds: string[];
		filteredCameraIds: string[];
		onclose: () => void;
	};
	const tabs = ['overview', 'defaults', 'templates', 'bulk'] as const;

	let { selectedCameraIds, filteredCameraIds, onclose }: Props = $props();
	const controlClient = useControlClient();
	const capabilities = useCapabilityState();

	let dialog = $state<HTMLDialogElement | null>(null);
	let closeButton = $state<HTMLButtonElement | null>(null);
	let activeTab = $state<Tab>(untrack(() => (selectedCameraIds.length > 0 ? 'bulk' : 'overview')));
	let snapshot = $state.raw<ConfigurationSnapshot | null>(null);
	let loading = $state(true);
	let busy = $state(false);
	let error = $state<string | null>(null);
	let mutationIssues = $state.raw<ConfigurationRequestError['issues']>([]);
	let conflictRevision = $state<string | null>(null);
	let search = $state('');
	let defaultsDraft = $state<PolicyPatchDraft>(emptyPolicyPatchDraft());
	let bulkDraft = $state<PolicyPatchDraft>(emptyPolicyPatchDraft());
	let bulkTargetMode = $state<BulkTargetMode>(
		untrack(() => (selectedCameraIds.length > 0 ? 'selected' : 'filtered'))
	);
	let bulkGroupId = $state('');
	let bulkChangeMode = $state<BulkChangeMode>('patch');
	let selectedTemplateId = $state('');
	let plan = $state.raw<ConfigurationPlan | null>(null);
	let applyResult = $state.raw<ConfigurationApplyResult | null>(null);
	let templateDraft = $state<TemplateDraft>(emptyTemplateDraft());
	let templateEditorOpen = $state(false);
	let deleteConfirmation = $state<string | null>(null);
	let importPreview = $state.raw<ConfigurationTemplateImportPreview | null>(null);
	let importDocument = $state('');
	let supported = $derived(capabilities.supports('keeppeek.configuration.v1'));
	let normalizedSearch = $derived(search.trim().toLocaleLowerCase());
	let filteredEvidence = $derived(
		(snapshot?.cameras ?? []).filter((entry) => {
			if (!normalizedSearch) return true;
			return [
				entry.camera.display_name,
				entry.camera.id,
				entry.camera.ip,
				entry.camera.model,
				...entry.group_ids,
				entry.backend.source,
				entry.transport.source,
				entry.recording_mode.source,
				'backend',
				'transport',
				'recording mode',
				'event window',
				'generic motion events'
			]
				.filter((value): value is string => Boolean(value))
				.some((value) => value.toLocaleLowerCase().includes(normalizedSearch));
		})
	);
	let filteredDomains = $derived(
		(snapshot?.domains ?? []).filter((domain) =>
			[domain.label, domain.domain_id, domain.capability_id, domain.owner_path]
				.join(' ')
				.toLocaleLowerCase()
				.includes(normalizedSearch)
		)
	);
	let filteredTemplates = $derived(
		(snapshot?.templates ?? []).filter((template) =>
			[template.name, template.description, template.template_id]
				.join(' ')
				.toLocaleLowerCase()
				.includes(normalizedSearch)
		)
	);
	let unsaved = $derived(
		policyPatchDraftDirty(defaultsDraft) ||
			policyPatchDraftDirty(bulkDraft) ||
			templateEditorOpen ||
			plan !== null ||
			importDocument.length > 0 ||
			importPreview !== null
	);

	beforeNavigate(({ cancel }) => {
		if (busy) {
			cancel();
			return;
		}
		if (unsaved && !window.confirm('Discard your unsaved configuration changes?')) cancel();
	});

	onMount(() => {
		dialog?.showModal();
		closeButton?.focus();
		void refreshSnapshot();
		return () => dialog?.close();
	});

	function requestClose(): void {
		if (busy) return;
		if (unsaved && !window.confirm('Discard your unsaved configuration changes?')) return;
		dialog?.close();
	}

	function protectReload(event: BeforeUnloadEvent): void {
		if (!unsaved && !busy) return;
		event.preventDefault();
		event.returnValue = '';
	}

	async function moveTab(event: KeyboardEvent, tab: Tab): Promise<void> {
		let nextIndex: number;
		if (event.key === 'Home') nextIndex = 0;
		else if (event.key === 'End') nextIndex = tabs.length - 1;
		else if (event.key === 'ArrowRight') nextIndex = (tabs.indexOf(tab) + 1) % tabs.length;
		else if (event.key === 'ArrowLeft') {
			nextIndex = (tabs.indexOf(tab) - 1 + tabs.length) % tabs.length;
		} else return;
		event.preventDefault();
		activeTab = tabs[nextIndex] ?? 'overview';
		plan = null;
		applyResult = null;
		await tick();
		document.getElementById(`configuration-tab-${activeTab}`)?.focus();
	}

	async function refreshSnapshot(preserveError = false): Promise<boolean> {
		if (!supported) {
			loading = false;
			return false;
		}
		try {
			const next = await controlClient.getConfigurationSnapshot();
			snapshot = next;
			if (!selectedTemplateId && next.templates[0]) {
				selectedTemplateId = next.templates[0].template_id;
			}
			conflictRevision = null;
			if (!preserveError) error = null;
			return true;
		} catch (cause) {
			error = messageFrom(cause, 'Configuration evidence could not be loaded.');
			return false;
		} finally {
			loading = false;
		}
	}

	async function captureMutationError(cause: unknown, fallback: string): Promise<void> {
		error = messageFrom(cause, fallback);
		mutationIssues = cause instanceof ConfigurationRequestError ? cause.issues : [];
		if (cause instanceof ConfigurationRequestError && cause.currentRevision) {
			conflictRevision = cause.currentRevision;
			await refreshSnapshot(true);
			conflictRevision = cause.currentRevision;
		}
	}

	async function previewDefaults(): Promise<void> {
		if (!snapshot || !supported || busy) return;
		busy = true;
		error = null;
		mutationIssues = [];
		applyResult = null;
		try {
			const patch = defaultPolicyPatch(defaultsDraft);
			if (Object.keys(patch).length === 0)
				throw new Error('Select at least one default to change.');
			plan = await controlClient.planConfigurationChange({
				expected_configuration_revision: snapshot.configuration_revision,
				change: { mode: 'defaults', patch }
			});
		} catch (cause) {
			await captureMutationError(cause, 'Shared defaults could not be previewed.');
		} finally {
			busy = false;
		}
	}

	async function previewBulk(): Promise<void> {
		if (!snapshot || !supported || busy) return;
		busy = true;
		error = null;
		mutationIssues = [];
		applyResult = null;
		try {
			const targets = selectedTargets();
			const change =
				bulkChangeMode === 'template'
					? { mode: 'template' as const, template_id: selectedTemplateId }
					: { mode: 'patch' as const, patch: cameraPolicyPatch(bulkDraft) };
			if (change.mode === 'template' && !change.template_id) {
				throw new Error('Select a template.');
			}
			if (change.mode === 'patch' && Object.keys(change.patch).length === 0) {
				throw new Error('Select at least one camera setting to change.');
			}
			plan = await controlClient.planConfigurationChange({
				expected_configuration_revision: snapshot.configuration_revision,
				targets,
				change
			});
		} catch (cause) {
			await captureMutationError(cause, 'Fleet changes could not be previewed.');
		} finally {
			busy = false;
		}
	}

	function selectedTargets(): ConfigurationTargetSelector {
		if (bulkTargetMode === 'selected') {
			if (selectedCameraIds.length === 0) throw new Error('Select at least one camera.');
			return { mode: 'camera-ids', camera_ids: selectedCameraIds };
		}
		if (bulkTargetMode === 'filtered') {
			if (filteredCameraIds.length === 0) throw new Error('The current filtered set is empty.');
			return { mode: 'camera-ids', camera_ids: filteredCameraIds };
		}
		if (bulkTargetMode === 'group') {
			const groupId = bulkGroupId.trim();
			if (!groupId) throw new Error('Enter a group ID.');
			return { mode: 'group', group_id: groupId };
		}
		return { mode: 'all-cameras' };
	}

	async function applyPlan(): Promise<void> {
		if (!plan || !supported || busy) return;
		busy = true;
		error = null;
		mutationIssues = [];
		try {
			const result = await controlClient.applyConfigurationPlan(
				plan.plan_id,
				plan.configuration_revision
			);
			applyResult = result;
			snapshot = result.snapshot;
			plan = null;
			if (activeTab === 'defaults') defaultsDraft = emptyPolicyPatchDraft();
			if (activeTab === 'bulk') bulkDraft = emptyPolicyPatchDraft();
			conflictRevision = null;
		} catch (cause) {
			await captureMutationError(cause, 'Configuration changes could not be applied.');
		} finally {
			busy = false;
		}
	}

	async function saveTemplate(): Promise<void> {
		if (!snapshot || !supported || busy) return;
		busy = true;
		error = null;
		mutationIssues = [];
		try {
			const template = templateFromDraft(templateDraft);
			await controlClient.saveConfigurationTemplate(
				snapshot.configuration_revision,
				template,
				templateDraft.template_id ? templateDraft.version : undefined
			);
			if (!(await refreshSnapshot())) return;
			templateEditorOpen = false;
			templateDraft = emptyTemplateDraft();
		} catch (cause) {
			await captureMutationError(cause, 'Template could not be saved.');
		} finally {
			busy = false;
		}
	}

	function editTemplate(template: ConfigurationTemplate): void {
		templateDraft = templateDraftFrom(template);
		templateEditorOpen = true;
		deleteConfirmation = null;
	}

	async function duplicateTemplate(template: ConfigurationTemplate): Promise<void> {
		if (!snapshot || !supported || busy) return;
		busy = true;
		error = null;
		mutationIssues = [];
		try {
			const names = new Set(snapshot.templates.map((entry) => entry.name.toLocaleLowerCase()));
			let suffix = 2;
			let name = `${template.name} copy`;
			while (names.has(name.toLocaleLowerCase())) {
				name = `${template.name} copy ${suffix}`;
				suffix += 1;
			}
			const duplicate = await controlClient.duplicateConfigurationTemplate(
				snapshot.configuration_revision,
				template.template_id,
				name
			);
			await refreshSnapshot();
			editTemplate(duplicate);
		} catch (cause) {
			await captureMutationError(cause, 'Template could not be duplicated.');
		} finally {
			busy = false;
		}
	}

	async function deleteTemplate(template: ConfigurationTemplate): Promise<void> {
		if (!snapshot || !supported || busy) return;
		if (deleteConfirmation !== template.template_id) {
			deleteConfirmation = template.template_id;
			return;
		}
		busy = true;
		error = null;
		mutationIssues = [];
		try {
			snapshot = await controlClient.deleteConfigurationTemplate(
				snapshot.configuration_revision,
				template.template_id
			);
			deleteConfirmation = null;
			if (selectedTemplateId === template.template_id) {
				selectedTemplateId = snapshot.templates[0]?.template_id ?? '';
			}
		} catch (cause) {
			await captureMutationError(cause, 'Template could not be deleted.');
		} finally {
			busy = false;
		}
	}

	async function exportTemplates(): Promise<void> {
		if (!supported || busy) return;
		busy = true;
		error = null;
		mutationIssues = [];
		try {
			const documentJson = await controlClient.exportConfigurationTemplates();
			const url = URL.createObjectURL(new Blob([documentJson], { type: 'application/json' }));
			const anchor = document.createElement('a');
			anchor.href = url;
			anchor.download = 'keeppeek-camera-templates.v1.json';
			anchor.click();
			URL.revokeObjectURL(url);
		} catch (cause) {
			error = messageFrom(cause, 'Templates could not be exported.');
		} finally {
			busy = false;
		}
	}

	async function readImport(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		if (!file || !snapshot || !supported) return;
		busy = true;
		error = null;
		mutationIssues = [];
		try {
			importDocument = await file.text();
			importPreview = await controlClient.previewConfigurationTemplateImport(
				snapshot.configuration_revision,
				importDocument
			);
		} catch (cause) {
			await captureMutationError(cause, 'Template import could not be previewed.');
		} finally {
			busy = false;
			input.value = '';
		}
	}

	async function applyImport(): Promise<void> {
		if (!snapshot || !importPreview || !supported || busy) return;
		busy = true;
		error = null;
		try {
			snapshot = await controlClient.applyConfigurationTemplateImport(
				importPreview.preview_id,
				importPreview.configuration_revision
			);
			importPreview = null;
			importDocument = '';
		} catch (cause) {
			await captureMutationError(cause, 'Template import could not be applied.');
		} finally {
			busy = false;
		}
	}

	function emptyTemplateDraft(): TemplateDraft {
		return {
			template_id: '',
			version: 0,
			name: '',
			description: '',
			username_secret_reference: '',
			password_secret_reference: '',
			onvif_port: '',
			http_port: '',
			backend: '',
			transport: '',
			record_generic_motion_events: '',
			recording_mode: '',
			event_recording_duration_secs: ''
		};
	}

	function templateDraftFrom(template: ConfigurationTemplate): TemplateDraft {
		return {
			template_id: template.template_id,
			version: template.version,
			name: template.name,
			description: template.description,
			username_secret_reference: template.values.username_secret_reference ?? '',
			password_secret_reference: template.values.password_secret_reference ?? '',
			onvif_port: template.values.onvif_port?.toString() ?? '',
			http_port: template.values.http_port?.toString() ?? '',
			backend: template.values.backend ?? '',
			transport: template.values.transport ?? '',
			record_generic_motion_events:
				template.values.record_generic_motion_events === undefined
					? ''
					: (template.values.record_generic_motion_events.toString() as 'true' | 'false'),
			recording_mode: template.values.recording_mode ?? '',
			event_recording_duration_secs: template.values.event_recording_duration_secs?.toString() ?? ''
		};
	}

	function templateFromDraft(draft: TemplateDraft): ConfigurationTemplate {
		const values: ConfigurationTemplateValues = {};
		if (draft.username_secret_reference.trim()) {
			values.username_secret_reference = draft.username_secret_reference.trim();
		}
		if (draft.password_secret_reference.trim()) {
			values.password_secret_reference = draft.password_secret_reference.trim();
		}
		if (draft.onvif_port)
			values.onvif_port = parseTemplateNumber(draft.onvif_port, 'ONVIF port', 65_535);
		if (draft.http_port)
			values.http_port = parseTemplateNumber(draft.http_port, 'HTTP port', 65_535);
		if (draft.backend) values.backend = draft.backend;
		if (draft.transport) values.transport = draft.transport;
		if (draft.record_generic_motion_events) {
			values.record_generic_motion_events = draft.record_generic_motion_events === 'true';
		}
		if (draft.recording_mode) values.recording_mode = draft.recording_mode;
		if (draft.event_recording_duration_secs) {
			values.event_recording_duration_secs = parseTemplateNumber(
				draft.event_recording_duration_secs,
				'Event recording duration',
				3_600
			);
		}
		return {
			template_id: draft.template_id,
			version: draft.version,
			name: draft.name.trim(),
			description: draft.description.trim(),
			values,
			created_at_ms: 0,
			updated_at_ms: 0
		};
	}

	function parseTemplateNumber(value: string, label: string, maximum: number): number {
		const number = Number(value);
		if (!Number.isSafeInteger(number) || number < 1 || number > maximum) {
			throw new Error(`${label} must be a whole number between 1 and ${maximum}.`);
		}
		return number;
	}

	function messageFrom(cause: unknown, fallback: string): string {
		return cause instanceof Error && cause.message.trim() ? cause.message : fallback;
	}

	function sourceLabel(source: string): string {
		return source.replace('-', ' ').toLocaleUpperCase();
	}

	function impactLabel(value: ConfigurationPlan['impact']): string {
		if (value === 'reconnect-camera') return 'RECONNECT CAMERA';
		if (value === 'restart-component') return 'RESTART COMPONENT';
		if (value === 'restart-server') return 'RESTART SERVER';
		return 'IMMEDIATE';
	}

	const selectClass =
		'h-9 w-full min-w-0 rounded-sm border border-hairline-strong bg-raised px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring';
</script>

<svelte:window onbeforeunload={protectReload} />

<dialog
	bind:this={dialog}
	class="m-auto h-[calc(100dvh-1rem)] w-[calc(100%-1rem)] max-w-[1180px] overflow-hidden rounded-md border border-hairline-strong bg-background p-0 text-foreground shadow-2xl backdrop:bg-black/60 md:h-[min(820px,calc(100dvh-2rem))]"
	aria-labelledby="fleet-configuration-title"
	oncancel={(event) => {
		event.preventDefault();
		requestClose();
	}}
	{onclose}
>
	<div class="grid h-full min-h-0 grid-rows-[auto_auto_minmax(0,1fr)]">
		<header class="flex items-start gap-3 border-b border-hairline-strong px-4 py-3 md:px-5">
			<div class="min-w-0 flex-1">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">CONFIGURATION V1</p>
				<h2 id="fleet-configuration-title" class="mt-1 text-lg font-semibold">
					Camera fleet configuration
				</h2>
				{#if snapshot}
					<p class="mt-1 truncate font-mono text-2xs text-text-faint">
						REV {snapshot.configuration_revision.slice(0, 12)} · {snapshot.cameras.length} CAMERAS
					</p>
				{/if}
				{#if unsaved}
					<p class="mt-1 font-mono text-2xs tracking-caps text-activity">UNSAVED DRAFT</p>
				{/if}
			</div>
			<Button
				bind:ref={closeButton}
				variant="ghost"
				size="icon"
				aria-label="Close configuration"
				onclick={requestClose}
				disabled={busy}
			>
				<XIcon />
			</Button>
		</header>

		<div
			class="flex min-w-0 items-center gap-2 overflow-x-auto border-b border-hairline px-3 py-2 md:px-5"
			role="tablist"
			aria-label="Configuration views"
		>
			{#each tabs as tab (tab)}
				<button
					id={`configuration-tab-${tab}`}
					type="button"
					role="tab"
					aria-selected={activeTab === tab}
					aria-controls="configuration-panel"
					tabindex={activeTab === tab ? 0 : -1}
					class="h-8 shrink-0 rounded-sm px-3 text-xs font-semibold capitalize focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {activeTab ===
					tab
						? 'bg-primary text-on-primary'
						: 'text-text-muted hover:bg-raised hover:text-foreground'}"
					onclick={() => {
						activeTab = tab as Tab;
						plan = null;
						applyResult = null;
					}}
					onkeydown={(event) => void moveTab(event, tab)}
				>
					{tab}
				</button>
			{/each}
			<div class="min-w-2 flex-1"></div>
			<Button
				variant="ghost"
				size="sm"
				onclick={() => void refreshSnapshot()}
				disabled={loading || busy}
			>
				<RefreshCwIcon /> Refresh
			</Button>
		</div>

		<div
			id="configuration-panel"
			class="min-h-0 overflow-y-auto"
			role="tabpanel"
			aria-labelledby={`configuration-tab-${activeTab}`}
			tabindex="0"
		>
			{#if !supported}
				<div
					class="border-b border-activity/40 bg-activity/10 px-4 py-3 text-sm text-activity"
					role="status"
				>
					Server update required · keeppeek.configuration.v1. Current fleet inventory remains
					available.
				</div>
			{/if}
			{#if error}
				<div
					class="border-b border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
					role="alert"
				>
					{error}
					{#if conflictRevision}
						<span class="ml-2 font-mono text-2xs">CURRENT {conflictRevision.slice(0, 12)}</span>
					{/if}
				</div>
				{#if mutationIssues.length > 0}
					<ul
						class="grid gap-1 border-b border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive"
					>
						{#each mutationIssues as issue (`${issue.cameraId ?? ''}:${issue.field}:${issue.code}`)}
							<li>
								<strong>{issue.field.replaceAll('_', ' ')}</strong> · {issue.message}
								{#if issue.cameraId}<span class="font-mono text-2xs"> · {issue.cameraId}</span>{/if}
							</li>
						{/each}
					</ul>
				{/if}
			{/if}
			{#if loading}
				<div class="grid min-h-72 place-items-center text-sm text-text-muted" aria-busy="true">
					Loading configuration evidence…
				</div>
			{:else if !snapshot}
				<div class="grid min-h-72 place-items-center px-4 text-center text-sm text-text-muted">
					Configuration evidence is unavailable.
				</div>
			{:else if plan}
				<section class="grid gap-5 p-4 md:p-5" aria-labelledby="configuration-review-heading">
					<div class="flex flex-wrap items-start gap-3">
						<div class="min-w-0 flex-1">
							<p class="font-mono text-2xs tracking-caps text-primary-soft">
								AUTHORITATIVE PREVIEW
							</p>
							<h3 id="configuration-review-heading" class="mt-1 text-base font-semibold">
								Review configuration changes
							</h3>
						</div>
						<span
							class="rounded-sm border border-hairline-strong bg-raised px-2 py-1 font-mono text-2xs tracking-caps"
							>{impactLabel(plan.impact)}</span
						>
						<span
							class="rounded-sm border border-hairline-strong bg-raised px-2 py-1 font-mono text-2xs tracking-caps"
							>{plan.authoritative_target_count} TARGETS</span
						>
					</div>
					<p class="text-sm text-text-muted">{plan.apply_semantics}</p>
					{#if plan.issues.length > 0}
						<div class="grid gap-2">
							{#each plan.issues as issue (`${issue.camera_id ?? ''}:${issue.field}:${issue.code}`)}
								<div
									class="flex gap-2 border-l-2 border-destructive bg-destructive/5 px-3 py-2 text-sm"
									role={issue.severity === 'error' ? 'alert' : 'status'}
								>
									<AlertTriangleIcon class="mt-0.5 size-4 shrink-0 text-destructive" />
									<span><strong>{issue.field}</strong> · {issue.message}</span>
								</div>
							{/each}
						</div>
					{/if}
					<div class="overflow-x-auto border-y border-hairline">
						<table class="w-full min-w-[760px] border-collapse text-left text-xs">
							<thead class="bg-raised font-mono text-2xs tracking-caps text-text-faint">
								<tr
									><th class="px-3 py-2">CAMERA</th><th class="px-3 py-2">FIELD</th><th
										class="px-3 py-2">CURRENT EFFECTIVE</th
									><th class="px-3 py-2">NEW EFFECTIVE</th><th class="px-3 py-2">SOURCE</th></tr
								>
							</thead>
							<tbody class="divide-y divide-hairline">
								{#each plan.changes as change (`${change.camera_id ?? 'default'}:${change.field}`)}
									<tr
										><td class="px-3 py-2 font-mono">{change.camera_id ?? 'Shared default'}</td><td
											class="px-3 py-2 font-medium">{change.field.replaceAll('_', ' ')}</td
										><td class="px-3 py-2 text-text-muted">{change.old_effective_value}</td><td
											class="px-3 py-2">{change.new_effective_value}</td
										><td class="px-3 py-2 font-mono text-2xs tracking-caps"
											>{sourceLabel(change.source)}</td
										></tr
									>
								{/each}
							</tbody>
						</table>
					</div>
					<details class="border-y border-hairline py-3">
						<summary class="cursor-pointer text-sm font-medium"
							>Exact target snapshot · {plan.targets.length}</summary
						>
						<ul class="mt-3 grid gap-1 text-xs text-text-muted lg:grid-cols-3 sm:grid-cols-2">
							{#each plan.targets as target (target.camera_id)}
								<li class="truncate">
									<span class="font-medium text-foreground">{target.display_name}</span> · {target.camera_id}{target.skipped
										? ` · ${target.skip_reason}`
										: ''}
								</li>
							{/each}
						</ul>
					</details>
					<div class="flex flex-wrap justify-end gap-2">
						<Button variant="outline" onclick={() => (plan = null)} disabled={busy}
							>Back to draft</Button
						>
						<Button onclick={() => void applyPlan()} disabled={!plan.valid || busy || !supported}
							>{busy ? 'Applying…' : 'Apply exact plan'}</Button
						>
					</div>
				</section>
			{:else if activeTab === 'overview'}
				<section class="grid gap-5 p-4 md:p-5" aria-labelledby="configuration-overview-heading">
					<div class="flex flex-wrap items-end gap-3">
						<div class="min-w-0 flex-1">
							<h3 id="configuration-overview-heading" class="text-base font-semibold">
								Effective configuration
							</h3>
						</div>
						<label class="relative w-full sm:w-80"
							><SearchIcon
								class="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-text-faint"
							/><span class="sr-only">Search configuration</span><Input
								type="search"
								class="pl-8"
								placeholder="Settings, cameras, capabilities, sources"
								bind:value={search}
							/></label
						>
					</div>
					<div
						class="grid gap-px overflow-hidden border-y border-hairline bg-hairline lg:grid-cols-5 sm:grid-cols-2"
					>
						<div class="bg-background p-3">
							<p class="font-mono text-2xs tracking-caps text-text-faint">BACKEND</p>
							<p class="mt-1 text-sm font-semibold">{snapshot.defaults.effective_backend}</p>
							<p class="mt-1 text-xs text-text-muted">
								{snapshot.defaults.configured_backend ? 'DEFAULT' : 'BUILT-IN'}
							</p>
						</div>
						<div class="bg-background p-3">
							<p class="font-mono text-2xs tracking-caps text-text-faint">TRANSPORT</p>
							<p class="mt-1 text-sm font-semibold">
								{snapshot.defaults.effective_transport.toUpperCase()}
							</p>
							<p class="mt-1 text-xs text-text-muted">
								{snapshot.defaults.configured_transport ? 'DEFAULT' : 'BUILT-IN'}
							</p>
						</div>
						<div class="bg-background p-3">
							<p class="font-mono text-2xs tracking-caps text-text-faint">RECORDING</p>
							<p class="mt-1 text-sm font-semibold">{snapshot.defaults.effective_recording_mode}</p>
							<p class="mt-1 text-xs text-text-muted">
								{snapshot.defaults.configured_recording_mode ? 'DEFAULT' : 'BUILT-IN'}
							</p>
						</div>
						<div class="bg-background p-3">
							<p class="font-mono text-2xs tracking-caps text-text-faint">EVENT WINDOW</p>
							<p class="mt-1 text-sm font-semibold">
								{snapshot.defaults.effective_event_recording_duration_secs}s
							</p>
							<p class="mt-1 text-xs text-text-muted">
								{snapshot.defaults.configured_event_recording_duration_secs !== null
									? 'DEFAULT'
									: 'BUILT-IN'}
							</p>
						</div>
						<div class="bg-background p-3">
							<p class="font-mono text-2xs tracking-caps text-text-faint">CREDENTIALS</p>
							<p class="mt-1 text-sm font-semibold">
								{snapshot.defaults.username_configured && snapshot.defaults.password_configured
									? 'Configured'
									: 'Incomplete'}
							</p>
							<p class="mt-1 text-xs text-text-muted">REFERENCES ONLY</p>
						</div>
					</div>
					<div class="overflow-x-auto border-y border-hairline">
						<table class="w-full min-w-[760px] border-collapse text-left text-xs">
							<thead class="bg-raised font-mono text-2xs tracking-caps text-text-faint"
								><tr
									><th class="px-3 py-2">CAMERA</th><th class="px-3 py-2">BACKEND</th><th
										class="px-3 py-2">TRANSPORT</th
									><th class="px-3 py-2">RECORDING</th><th class="px-3 py-2">EVENT WINDOW</th><th
										class="px-3 py-2">APPLIED</th
									></tr
								></thead
							>
							<tbody class="divide-y divide-hairline">
								{#each filteredEvidence as entry (entry.camera.id)}
									<tr
										><td class="px-3 py-2"
											><p class="font-medium">{entry.camera.display_name ?? entry.camera.id}</p>
											<p class="font-mono text-2xs text-text-faint">{entry.camera.id}</p></td
										><td class="px-3 py-2"
											>{entry.backend.effective}<span
												class="ml-2 font-mono text-2xs text-text-faint"
												>{sourceLabel(entry.backend.source)}</span
											></td
										><td class="px-3 py-2"
											>{entry.transport.effective.toUpperCase()}<span
												class="ml-2 font-mono text-2xs text-text-faint"
												>{sourceLabel(entry.transport.source)}</span
											></td
										><td class="px-3 py-2"
											>{entry.recording_mode.effective}<span
												class="ml-2 font-mono text-2xs text-text-faint"
												>{sourceLabel(entry.recording_mode.source)}</span
											></td
										><td class="px-3 py-2">{entry.event_recording_duration_secs.effective}s</td><td
											class="px-3 py-2"
											>{entry.backend.runtime_applied &&
											entry.transport.runtime_applied &&
											entry.recording_mode.runtime_applied
												? 'CURRENT'
												: 'PENDING'}</td
										></tr
									>
								{/each}
							</tbody>
						</table>
					</div>
					<div>
						<h3 class="text-sm font-semibold">Configuration ownership</h3>
						<div
							class="mt-2 grid gap-px overflow-hidden border-y border-hairline bg-hairline lg:grid-cols-3 sm:grid-cols-2"
						>
							{#each filteredDomains as domain (domain.domain_id)}<a
									href={domain.owner_path}
									class="bg-background p-3 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
									><span class="text-sm font-medium">{domain.label}</span><span
										class="mt-1 block font-mono text-2xs text-text-faint"
										>{domain.mutable ? domain.capability_id : domain.unavailable_reason}</span
									></a
								>{/each}
						</div>
					</div>
				</section>
			{:else if activeTab === 'defaults'}
				<section class="grid gap-5 p-4 md:p-5" aria-labelledby="configuration-defaults-heading">
					<div>
						<h3 id="configuration-defaults-heading" class="text-base font-semibold">
							Shared camera defaults
						</h3>
						<p class="mt-1 text-sm text-text-muted">
							Revision {snapshot.configuration_revision.slice(0, 12)}
						</p>
					</div>
					<ConfigurationPolicyFields
						bind:draft={defaultsDraft}
						includeCredentials={true}
						includePorts={false}
						clearLabel="Use built-in value"
					/>
					<div class="flex justify-end">
						<Button onclick={() => void previewDefaults()} disabled={busy || !supported}
							>{busy ? 'Preparing…' : 'Preview default changes'}</Button
						>
					</div>
				</section>
			{:else if activeTab === 'templates'}
				<section class="grid gap-5 p-4 md:p-5" aria-labelledby="configuration-templates-heading">
					<div class="flex flex-wrap items-end gap-2">
						<div class="min-w-0 flex-1">
							<h3 id="configuration-templates-heading" class="text-base font-semibold">
								Camera templates
							</h3>
							<p class="mt-1 text-sm text-text-muted">
								{snapshot.templates.length} of {snapshot.limits.maximum_templates}
							</p>
						</div>
						<Button
							variant="outline"
							onclick={() => void exportTemplates()}
							disabled={busy || !supported}><DownloadIcon /> Export</Button
						><label
							class="inline-flex h-9 cursor-pointer items-center gap-2 rounded-sm border border-hairline-strong bg-background px-3 text-sm font-medium focus-within:ring-2 focus-within:ring-ring"
							><FileUpIcon class="size-4" /> Import<input
								type="file"
								accept="application/json,.json"
								class="sr-only"
								onchange={(event) => void readImport(event)}
								disabled={busy || !supported}
							/></label
						><Button
							onclick={() => {
								templateDraft = emptyTemplateDraft();
								templateEditorOpen = true;
							}}
							disabled={!supported}><PlusIcon /> New template</Button
						>
					</div>
					{#if importPreview}<div class="border-y border-hairline p-3">
							<div class="flex flex-wrap items-center gap-2">
								<p class="min-w-0 flex-1 text-sm font-medium">
									Import preview · {importPreview.templates.length} templates
								</p>
								<Button
									variant="outline"
									onclick={() => {
										importPreview = null;
										importDocument = '';
									}}>Cancel</Button
								><Button
									onclick={() => void applyImport()}
									disabled={!importPreview.valid || busy || !supported}>Apply import</Button
								>
							</div>
							{#each importPreview.issues as issue (`${issue.field}:${issue.code}`)}<p
									class="mt-2 text-sm text-destructive"
									role="alert"
								>
									{issue.field} · {issue.message}
								</p>{/each}
						</div>{/if}
					{#if templateEditorOpen}<form
							class="grid gap-4 border-y border-hairline py-4"
							onsubmit={(event) => {
								event.preventDefault();
								void saveTemplate();
							}}
						>
							<div class="grid gap-3 sm:grid-cols-2">
								<label class="grid gap-1 text-sm font-medium" for="template-name"
									>Name<Input
										id="template-name"
										bind:value={templateDraft.name}
										maxlength={snapshot.limits.maximum_template_name_bytes}
										required
									/></label
								><label class="grid gap-1 text-sm font-medium" for="template-description"
									>Description<Input
										id="template-description"
										bind:value={templateDraft.description}
										maxlength={snapshot.limits.maximum_template_description_bytes}
									/></label
								>
							</div>
							<div class="grid gap-3 lg:grid-cols-3 sm:grid-cols-2">
								<label class="grid gap-1 text-sm font-medium" for="template-backend"
									>Backend<select
										id="template-backend"
										class={selectClass}
										bind:value={templateDraft.backend}
										><option value="">Not included</option><option value="auto">Auto</option><option
											value="retina">Retina</option
										><option value="reo-proto">Reo-Proto</option></select
									></label
								><label class="grid gap-1 text-sm font-medium" for="template-transport"
									>Transport<select
										id="template-transport"
										class={selectClass}
										bind:value={templateDraft.transport}
										><option value="">Not included</option><option value="tcp">TCP</option><option
											value="udp">UDP</option
										></select
									></label
								><label class="grid gap-1 text-sm font-medium" for="template-recording"
									>Recording mode<select
										id="template-recording"
										class={selectClass}
										bind:value={templateDraft.recording_mode}
										><option value="">Not included</option><option value="off">Off</option><option
											value="sub">Sub</option
										><option value="main">Main</option><option value="both">Both</option><option
											value="event-boost">Event boost</option
										></select
									></label
								><label class="grid gap-1 text-sm font-medium" for="template-duration"
									>Event window<Input
										id="template-duration"
										type="number"
										min="1"
										max="3600"
										bind:value={templateDraft.event_recording_duration_secs}
										placeholder="Not included"
									/></label
								><label class="grid gap-1 text-sm font-medium" for="template-motion"
									>Generic motion events<select
										id="template-motion"
										class={selectClass}
										bind:value={templateDraft.record_generic_motion_events}
										><option value="">Not included</option><option value="false"
											>Do not store</option
										><option value="true">Store</option></select
									></label
								><label class="grid gap-1 text-sm font-medium" for="template-onvif"
									>ONVIF port<Input
										id="template-onvif"
										type="number"
										min="1"
										max="65535"
										bind:value={templateDraft.onvif_port}
										placeholder="Not included"
									/></label
								><label class="grid gap-1 text-sm font-medium" for="template-http"
									>HTTP port<Input
										id="template-http"
										type="number"
										min="1"
										max="65535"
										bind:value={templateDraft.http_port}
										placeholder="Not included"
									/></label
								><label class="grid gap-1 text-sm font-medium" for="template-username"
									>Username reference<Input
										id="template-username"
										bind:value={templateDraft.username_secret_reference}
										placeholder={'{secret:CAMERA_USERNAME}'}
										autocomplete="off"
									/></label
								><label class="grid gap-1 text-sm font-medium" for="template-password"
									>Password reference<Input
										id="template-password"
										type="password"
										bind:value={templateDraft.password_secret_reference}
										placeholder={'{secret:CAMERA_PASSWORD}'}
										autocomplete="new-password"
									/></label
								>
							</div>
							<div class="flex justify-end gap-2">
								<Button
									type="button"
									variant="outline"
									onclick={() => (templateEditorOpen = false)}
									disabled={busy}>Cancel</Button
								><Button type="submit" disabled={busy || !supported}
									>{busy
										? 'Saving…'
										: templateDraft.template_id
											? 'Save template'
											: 'Create template'}</Button
								>
							</div>
						</form>{/if}
					<label class="relative max-w-sm"
						><SearchIcon
							class="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-text-faint"
						/><span class="sr-only">Search templates</span><Input
							type="search"
							class="pl-8"
							placeholder="Search templates"
							bind:value={search}
						/></label
					>
					<div class="grid gap-px overflow-hidden border-y border-hairline bg-hairline">
						{#each filteredTemplates as template (template.template_id)}<article
								class="grid gap-3 bg-background p-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center"
							>
								<div class="min-w-0">
									<h4 class="text-sm font-semibold">{template.name}</h4>
									<p class="mt-1 text-xs text-text-muted">
										{template.description || 'No description'} · VERSION {template.version}
									</p>
									<p class="mt-2 font-mono text-2xs tracking-caps text-text-faint">
										{Object.keys(template.values)
											.map((field) => field.replaceAll('_', ' '))
											.join(' · ')}
									</p>
								</div>
								<div class="flex flex-wrap gap-2">
									<Button variant="outline" size="sm" onclick={() => editTemplate(template)}
										>Edit</Button
									><Button
										variant="outline"
										size="sm"
										onclick={() => void duplicateTemplate(template)}
										disabled={busy || !supported}><CopyIcon /> Duplicate</Button
									><Button
										variant={deleteConfirmation === template.template_id
											? 'destructive'
											: 'outline'}
										size="sm"
										onclick={() => void deleteTemplate(template)}
										disabled={busy || !supported}
										><TrashIcon />
										{deleteConfirmation === template.template_id
											? 'Confirm delete'
											: 'Delete'}</Button
									>
								</div>
							</article>{/each}
					</div>
				</section>
			{:else}
				<section class="grid gap-5 p-4 md:p-5" aria-labelledby="configuration-bulk-heading">
					<div>
						<h3 id="configuration-bulk-heading" class="text-base font-semibold">
							Bulk camera changes
						</h3>
						<p class="mt-1 text-sm text-text-muted">
							Targets are resolved again by the server at preview time.
						</p>
					</div>
					<div class="grid gap-3 sm:grid-cols-2">
						<label class="grid gap-1 text-sm font-medium" for="bulk-target-mode"
							>Target set<select
								id="bulk-target-mode"
								class={selectClass}
								bind:value={bulkTargetMode}
								><option value="selected" disabled={selectedCameraIds.length === 0}
									>Selected cameras ({selectedCameraIds.length})</option
								><option value="filtered" disabled={filteredCameraIds.length === 0}
									>Current filtered set ({filteredCameraIds.length})</option
								><option value="group">Group</option><option value="all"
									>All cameras ({snapshot.cameras.length})</option
								></select
							></label
						>{#if bulkTargetMode === 'group'}<label
								class="grid gap-1 text-sm font-medium"
								for="bulk-group-id"
								>Group ID<Input id="bulk-group-id" bind:value={bulkGroupId} /></label
							>{/if}<label class="grid gap-1 text-sm font-medium" for="bulk-change-mode"
							>Change source<select
								id="bulk-change-mode"
								class={selectClass}
								bind:value={bulkChangeMode}
								><option value="patch">Named fields</option><option value="template"
									>Template</option
								></select
							></label
						>{#if bulkChangeMode === 'template'}<label
								class="grid gap-1 text-sm font-medium"
								for="bulk-template"
								>Template<select
									id="bulk-template"
									class={selectClass}
									bind:value={selectedTemplateId}
									><option value="">Select template</option
									>{#each snapshot.templates as template (template.template_id)}<option
											value={template.template_id}>{template.name} · v{template.version}</option
										>{/each}</select
								></label
							>{/if}
					</div>
					{#if bulkChangeMode === 'patch'}<ConfigurationPolicyFields
							bind:draft={bulkDraft}
							includeCredentials={true}
							includePorts={true}
						/>{/if}
					<div class="flex justify-end">
						<Button onclick={() => void previewBulk()} disabled={busy || !supported}
							>{busy ? 'Preparing…' : 'Preview exact changes'}</Button
						>
					</div>
				</section>
			{/if}
			{#if applyResult}
				<section
					class="border-t border-hairline p-4 md:p-5"
					aria-labelledby="configuration-activation-heading"
				>
					<div class="flex items-center gap-2">
						<CheckIcon class="text-success size-4" />
						<h3 id="configuration-activation-heading" class="text-sm font-semibold">
							Configuration committed
						</h3>
						<span class="font-mono text-2xs tracking-caps text-text-faint"
							>{impactLabel(applyResult.impact)}</span
						>
					</div>
					<div class="mt-3 grid gap-2 lg:grid-cols-3 sm:grid-cols-2">
						{#each applyResult.activations as activation (activation.camera_id)}<div
								class="border-l-2 {activation.status === 'failed'
									? 'border-destructive'
									: activation.status === 'applied'
										? 'border-success'
										: 'border-activity'} bg-raised px-3 py-2"
							>
								<p class="font-mono text-xs">{activation.camera_id}</p>
								<p class="mt-1 text-xs font-semibold">
									{activation.status.replaceAll('-', ' ').toUpperCase()}
								</p>
								{#if activation.detail}<p class="mt-1 text-xs text-text-muted">
										{activation.detail}
									</p>{/if}
							</div>{/each}
					</div>
				</section>
			{/if}
		</div>
	</div>
</dialog>
