<script lang="ts">
	import { beforeNavigate } from '$app/navigation';
	import { tick, untrack } from 'svelte';
	import type {
		DiskHealth,
		SanitizedConfig,
		ServerHealthResponse,
		SettingsConfigUpdate
	} from '$lib/types';
	import {
		catalogRecordingBytes,
		effectiveStoragePolicy,
		mostSpecificDiskForPath,
		suggestedStorageDisks
	} from '$lib/storage-retention';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import ArrowLeftIcon from '@lucide/svelte/icons/arrow-left';
	import ArrowRightIcon from '@lucide/svelte/icons/arrow-right';
	import CheckCircle2Icon from '@lucide/svelte/icons/circle-check';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import HardDriveIcon from '@lucide/svelte/icons/hard-drive';
	import MoveIcon from '@lucide/svelte/icons/move-right';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SaveIcon from '@lucide/svelte/icons/save';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';

	type StorageDraft = {
		mediumTermPath: string;
		longTermPath: string;
		recordingCatalogPath: string;
		eventThumbnailPath: string;
		eventThumbnailMaxMegabytes: string;
		shortTermSeconds: string;
		mediumTermSeconds: string;
		flushIntervalSeconds: string;
		writeBufferBytes: string;
		longTermMaxGigabytes: string;
		minimumFreeGigabytes: string;
		maximumUsedPercent: string;
		warningFreeGigabytes: string;
		criticalFreeGigabytes: string;
		cleanupHysteresisGigabytes: string;
	};

	type FieldName = keyof StorageDraft;
	type MigrationChoice = 'unselected' | 'leave' | 'move';
	type Step = 'configure' | 'review';
	type Change = { label: string; from: string; to: string };

	type Props = {
		config: SanitizedConfig;
		health: ServerHealthResponse | null;
		saving?: boolean;
		error?: string | null;
		oncancel: () => void;
		onsave: (update: SettingsConfigUpdate) => void | Promise<void>;
	};

	let { config, health, saving = false, error = null, oncancel, onsave }: Props = $props();

	const GIBIBYTE_BYTES = 1_073_741_824;
	const MAX_WRITE_BUFFER_BYTES = 64 * 1_024 * 1_024;
	const advancedFields: readonly FieldName[] = [
		'mediumTermPath',
		'recordingCatalogPath',
		'eventThumbnailPath',
		'eventThumbnailMaxMegabytes',
		'shortTermSeconds',
		'mediumTermSeconds',
		'flushIntervalSeconds',
		'writeBufferBytes'
	];
	const initialDraft = untrack(() => storageDraftFromConfig(config));
	let draft = $state<StorageDraft>({ ...initialDraft });
	let migrationChoice = $state<MigrationChoice>('unselected');
	let confirmUnlimited = $state(false);
	let step = $state<Step>('configure');
	let reviewHeading = $state<HTMLHeadingElement | null>(null);

	let disks = $derived(health?.system.disks ?? []);
	let suggestedDisks = $derived(suggestedStorageDisks(disks, config.storage.long_term_path));
	let sourceDisk = $derived(mostSpecificDiskForPath(config.storage.long_term_path, disks));
	let destinationDisk = $derived(mostSpecificDiskForPath(draft.longTermPath, disks));
	let locationsChanged = $derived(
		draft.mediumTermPath.trim() !== initialDraft.mediumTermPath ||
			draft.longTermPath.trim() !== initialDraft.longTermPath ||
			draft.recordingCatalogPath.trim() !== initialDraft.recordingCatalogPath ||
			draft.eventThumbnailPath.trim() !== initialDraft.eventThumbnailPath
	);
	let fieldErrors = $derived(validateDraft(draft, confirmUnlimited));
	let advancedErrorCount = $derived(
		advancedFields.filter((field) => fieldErrors[field] !== null).length
	);
	let migrationError = $derived(validateMigration());
	let policyError = $derived(validatePolicy());
	let hasErrors = $derived(
		Object.values(fieldErrors).some((message) => message !== null) ||
			migrationError !== null ||
			policyError !== null
	);
	let dirty = $derived(
		(Object.keys(initialDraft) as FieldName[]).some(
			(field) => draft[field] !== initialDraft[field]
		) ||
			(locationsChanged && migrationChoice !== 'unselected')
	);
	let proposedPolicy = $derived.by(evaluateDraftPolicy);
	let proposedRetentionDays = $derived(retentionDaysForLimit(proposedPolicy?.effectiveLimitBytes));
	let estimatedPruneBytes = $derived.by(() => {
		const effectiveLimit = proposedPolicy?.effectiveLimitBytes;
		const indexed = catalogRecordingBytes(health?.storage.catalog);
		if (effectiveLimit === null || effectiveLimit === undefined || indexed === null) return 0;
		return Math.max(0, indexed - effectiveLimit);
	});
	let changes = $derived.by(buildChanges);

	beforeNavigate(({ cancel }) => {
		if (!dirty || saving) return;
		if (!window.confirm('Discard your unsaved storage changes?')) cancel();
	});

	function storageDraftFromConfig(value: SanitizedConfig): StorageDraft {
		return {
			mediumTermPath: value.storage.medium_term_path,
			longTermPath: value.storage.long_term_path,
			recordingCatalogPath: value.storage.recording_catalog_path,
			eventThumbnailPath: value.storage.event_thumbnail_path,
			eventThumbnailMaxMegabytes: value.storage.event_thumbnail_max_mb.toString(),
			shortTermSeconds: value.storage.short_term_secs.toString(),
			mediumTermSeconds: value.storage.medium_term_secs.toString(),
			flushIntervalSeconds: value.storage.flush_interval_secs.toString(),
			writeBufferBytes: value.storage.write_buffer_bytes.toString(),
			longTermMaxGigabytes: value.storage.long_term_max_gb.toString(),
			minimumFreeGigabytes: (value.storage.minimum_free_gb ?? 0).toString(),
			maximumUsedPercent: value.storage.maximum_used_percent?.toString() ?? '',
			warningFreeGigabytes: (value.storage.warning_free_gb ?? 0).toString(),
			criticalFreeGigabytes: (value.storage.critical_free_gb ?? 0).toString(),
			cleanupHysteresisGigabytes: (value.storage.cleanup_hysteresis_gb ?? 0).toString()
		};
	}

	function childPath(root: string, name: string): string {
		const separator = root.includes('\\') && !root.includes('/') ? '\\' : '/';
		return `${root.replace(/[\\/]+$/, '')}${separator}${name}`;
	}

	function setRecordingLocation(value: string): void {
		const previous = draft.longTermPath;
		if (draft.mediumTermPath === previous) draft.mediumTermPath = value;
		if (draft.recordingCatalogPath === childPath(previous, 'recordings.db')) {
			draft.recordingCatalogPath = childPath(value, 'recordings.db');
		}
		if (draft.eventThumbnailPath === childPath(previous, '.event-thumbnails')) {
			draft.eventThumbnailPath = childPath(value, '.event-thumbnails');
		}
		draft.longTermPath = value;
	}

	function wholeNumberError(
		value: string,
		label: string,
		minimum: number,
		maximum: number
	): string | null {
		const trimmed = String(value).trim();
		if (!/^\d+$/.test(trimmed)) return `${label} must be a whole number.`;
		const number = Number(trimmed);
		if (!Number.isSafeInteger(number) || number < minimum || number > maximum) {
			return `${label} must be between ${minimum} and ${maximum}.`;
		}
		return null;
	}

	function pathError(value: string, label: string): string | null {
		if (!value.trim()) return `${label} is required.`;
		if (value.includes('\0')) return `${label} contains an invalid character.`;
		return null;
	}

	function optionalPercentError(value: string): string | null {
		if (!String(value).trim()) return null;
		return wholeNumberError(value, 'Maximum filesystem usage', 1, 99);
	}

	function allSafetyLimitsDisabled(value: StorageDraft): boolean {
		return (
			String(value.longTermMaxGigabytes).trim() === '0' &&
			String(value.minimumFreeGigabytes).trim() === '0' &&
			!String(value.maximumUsedPercent).trim() &&
			String(value.warningFreeGigabytes).trim() === '0' &&
			String(value.criticalFreeGigabytes).trim() === '0'
		);
	}

	function validateDraft(value: StorageDraft, unlimitedConfirmed: boolean) {
		const errors: Record<FieldName, string | null> = {
			mediumTermPath: pathError(value.mediumTermPath, 'Active recording path'),
			longTermPath: pathError(value.longTermPath, 'Recording location'),
			recordingCatalogPath: pathError(value.recordingCatalogPath, 'Recording catalog path'),
			eventThumbnailPath: pathError(value.eventThumbnailPath, 'Event thumbnail path'),
			eventThumbnailMaxMegabytes: wholeNumberError(
				value.eventThumbnailMaxMegabytes,
				'Thumbnail storage limit',
				0,
				Number.MAX_SAFE_INTEGER
			),
			shortTermSeconds: wholeNumberError(
				value.shortTermSeconds,
				'Memory buffer',
				0,
				Number.MAX_SAFE_INTEGER
			),
			mediumTermSeconds: wholeNumberError(
				value.mediumTermSeconds,
				'Recording file duration',
				0,
				Number.MAX_SAFE_INTEGER
			),
			flushIntervalSeconds: wholeNumberError(
				value.flushIntervalSeconds,
				'Flush interval',
				0,
				Number.MAX_SAFE_INTEGER
			),
			writeBufferBytes: wholeNumberError(
				value.writeBufferBytes,
				'Write buffer',
				1,
				MAX_WRITE_BUFFER_BYTES
			),
			longTermMaxGigabytes: wholeNumberError(
				value.longTermMaxGigabytes,
				'Maximum recording storage',
				0,
				Number.MAX_SAFE_INTEGER
			),
			minimumFreeGigabytes: wholeNumberError(
				value.minimumFreeGigabytes,
				'Minimum free space',
				0,
				Number.MAX_SAFE_INTEGER
			),
			maximumUsedPercent: optionalPercentError(value.maximumUsedPercent),
			warningFreeGigabytes: wholeNumberError(
				value.warningFreeGigabytes,
				'Warning free space',
				0,
				Number.MAX_SAFE_INTEGER
			),
			criticalFreeGigabytes: wholeNumberError(
				value.criticalFreeGigabytes,
				'Critical free space',
				0,
				Number.MAX_SAFE_INTEGER
			),
			cleanupHysteresisGigabytes: wholeNumberError(
				value.cleanupHysteresisGigabytes,
				'Cleanup hysteresis',
				0,
				Number.MAX_SAFE_INTEGER
			)
		};
		if (allSafetyLimitsDisabled(value) && !unlimitedConfirmed) {
			errors.longTermMaxGigabytes = 'Confirm unbounded storage before continuing.';
		}
		return errors;
	}

	function validateMigration(): string | null {
		if (!locationsChanged) return null;
		const indexedBytes = catalogRecordingBytes(health?.storage.catalog) ?? 0;
		if (indexedBytes > 0 && migrationChoice === 'unselected') {
			return 'Choose whether to leave or move the indexed recordings before continuing.';
		}
		if (migrationChoice !== 'move') return null;
		const changedDestinations = [
			[initialDraft.mediumTermPath, draft.mediumTermPath],
			[initialDraft.longTermPath, draft.longTermPath],
			[initialDraft.recordingCatalogPath, draft.recordingCatalogPath],
			[initialDraft.eventThumbnailPath, draft.eventThumbnailPath]
		]
			.filter(([from, to]) => from !== String(to).trim())
			.map(([, to]) => String(to).trim());
		const unreportedPath = changedDestinations.find(
			(path) => mostSpecificDiskForPath(path, disks) === null
		);
		if (unreportedPath) {
			return `The destination for ${unreportedPath} is not present in the current health report. Choose a reported mount or leave existing files in place.`;
		}
		if (!destinationDisk) return null;
		if (!sourceDisk || sourceDisk.mount_point === destinationDisk.mount_point) return null;
		if (indexedBytes > destinationDisk.available_bytes) {
			return `The destination has ${formatBytes(destinationDisk.available_bytes)} free, less than the ${formatBytes(indexedBytes)} indexed archive.`;
		}
		return null;
	}

	function evaluateDraftPolicy() {
		const numericFields = [
			draft.longTermMaxGigabytes,
			draft.minimumFreeGigabytes,
			draft.warningFreeGigabytes,
			draft.criticalFreeGigabytes,
			draft.cleanupHysteresisGigabytes
		];
		if (numericFields.some((value) => !/^\d+$/.test(String(value).trim()))) return null;
		const maximumUsed = String(draft.maximumUsedPercent).trim();
		if (maximumUsed && !/^\d+$/.test(maximumUsed)) return null;
		return effectiveStoragePolicy({
			archiveMaxBytes: parseWholeNumber(draft.longTermMaxGigabytes) * GIBIBYTE_BYTES,
			minimumFreeBytes: parseWholeNumber(draft.minimumFreeGigabytes) * GIBIBYTE_BYTES,
			maximumUsedPercent: maximumUsed ? Number(maximumUsed) : null,
			warningFreeBytes: parseWholeNumber(draft.warningFreeGigabytes) * GIBIBYTE_BYTES,
			criticalFreeBytes: parseWholeNumber(draft.criticalFreeGigabytes) * GIBIBYTE_BYTES,
			cleanupHysteresisBytes: parseWholeNumber(draft.cleanupHysteresisGigabytes) * GIBIBYTE_BYTES,
			capacity: destinationDisk,
			keeppeekBytes: catalogRecordingBytes(health?.storage.catalog) ?? 0
		});
	}

	function validatePolicy(): string | null {
		const values = [
			draft.minimumFreeGigabytes,
			draft.warningFreeGigabytes,
			draft.criticalFreeGigabytes,
			draft.cleanupHysteresisGigabytes
		];
		if (values.some((value) => !/^\d+$/.test(String(value).trim()))) return null;
		const minimumFree = parseWholeNumber(draft.minimumFreeGigabytes);
		const criticalFree = Math.max(minimumFree, parseWholeNumber(draft.criticalFreeGigabytes));
		const warningFree = parseWholeNumber(draft.warningFreeGigabytes);
		if (warningFree > 0 && warningFree < criticalFree) {
			return 'Warning free space must be greater than or equal to critical free space.';
		}
		if (!proposedPolicy || !destinationDisk) return null;
		if (
			proposedPolicy.warningFreeBytes > destinationDisk.total_bytes ||
			proposedPolicy.recoveryFreeBytes > destinationDisk.total_bytes
		) {
			return 'The proposed headroom thresholds exceed the destination filesystem capacity.';
		}
		const indexedBytes = catalogRecordingBytes(health?.storage.catalog) ?? 0;
		const reclaimableBytes =
			draft.longTermPath.trim() === initialDraft.longTermPath || migrationChoice === 'move'
				? indexedBytes
				: 0;
		if (destinationDisk.available_bytes + reclaimableBytes < proposedPolicy.recoveryFreeBytes) {
			return 'The destination cannot provide the proposed cleanup recovery headroom.';
		}
		return null;
	}

	function parseWholeNumber(value: string): number {
		return Number(String(value).trim());
	}

	function retentionDaysForLimit(
		limitBytes: number | null | undefined
	): number | null | 'unlimited' {
		if (limitBytes === undefined) return null;
		if (limitBytes === null) return 'unlimited';
		if (config.recording_estimate.bytes_per_day <= 0) return null;
		return limitBytes / config.recording_estimate.bytes_per_day;
	}

	function buildChanges(): Change[] {
		const labels: Record<FieldName, string> = {
			mediumTermPath: 'Active recording path',
			longTermPath: 'Recording location',
			recordingCatalogPath: 'Recording catalog',
			eventThumbnailPath: 'Event thumbnails',
			eventThumbnailMaxMegabytes: 'Thumbnail limit',
			shortTermSeconds: 'Memory buffer',
			mediumTermSeconds: 'Recording file duration',
			flushIntervalSeconds: 'Flush interval',
			writeBufferBytes: 'Write buffer',
			longTermMaxGigabytes: 'Archive limit',
			minimumFreeGigabytes: 'Minimum free space',
			maximumUsedPercent: 'Maximum filesystem usage',
			warningFreeGigabytes: 'Warning free space',
			criticalFreeGigabytes: 'Critical free space',
			cleanupHysteresisGigabytes: 'Cleanup hysteresis'
		};
		return (Object.keys(initialDraft) as FieldName[])
			.filter((field) => draft[field] !== initialDraft[field])
			.map((field) => ({ label: labels[field], from: initialDraft[field], to: draft[field] }));
	}

	function updateFromDraft(): SettingsConfigUpdate {
		return {
			host: config.host,
			port: config.port,
			expected_configuration_revision: config.configuration_revision,
			move_existing_recordings: locationsChanged && migrationChoice === 'move',
			storage: {
				medium_term_path: draft.mediumTermPath.trim(),
				long_term_path: draft.longTermPath.trim(),
				recording_catalog_path: draft.recordingCatalogPath.trim(),
				event_thumbnail_path: draft.eventThumbnailPath.trim(),
				event_thumbnail_max_mb: parseWholeNumber(draft.eventThumbnailMaxMegabytes),
				short_term_secs: parseWholeNumber(draft.shortTermSeconds),
				medium_term_secs: parseWholeNumber(draft.mediumTermSeconds),
				flush_interval_secs: parseWholeNumber(draft.flushIntervalSeconds),
				write_buffer_bytes: parseWholeNumber(draft.writeBufferBytes),
				long_term_max_gb: parseWholeNumber(draft.longTermMaxGigabytes),
				minimum_free_gb: parseWholeNumber(draft.minimumFreeGigabytes),
				maximum_used_percent: String(draft.maximumUsedPercent).trim()
					? parseWholeNumber(draft.maximumUsedPercent)
					: null,
				warning_free_gb: parseWholeNumber(draft.warningFreeGigabytes),
				critical_free_gb: parseWholeNumber(draft.criticalFreeGigabytes),
				cleanup_hysteresis_gb: parseWholeNumber(draft.cleanupHysteresisGigabytes)
			}
		};
	}

	async function continueToReview(): Promise<void> {
		if (!dirty || hasErrors) return;
		step = 'review';
		await tick();
		reviewHeading?.focus();
	}

	function submit(event: SubmitEvent): void {
		event.preventDefault();
		if (saving) return;
		if (step === 'configure') {
			void continueToReview();
			return;
		}
		if (hasErrors || !dirty) return;
		void onsave(updateFromDraft());
	}

	function requestCancel(): void {
		if (dirty && !window.confirm('Discard your unsaved storage changes?')) return;
		oncancel();
	}

	function protectReload(event: BeforeUnloadEvent): void {
		if (!dirty || saving) return;
		event.preventDefault();
		event.returnValue = '';
	}

	function formatBytes(bytes: number): string {
		if (!Number.isFinite(bytes) || bytes < 0) return 'Unavailable';
		const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
		let value = bytes;
		let unitIndex = 0;
		while (value >= 1_024 && unitIndex < units.length - 1) {
			value /= 1_024;
			unitIndex += 1;
		}
		return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: value >= 10 ? 0 : 1 }).format(value)} ${units[unitIndex]}`;
	}

	function formatRetention(value: number | null | 'unlimited'): string {
		if (value === 'unlimited') return 'Unlimited';
		if (value === null) return 'Unavailable';
		if (value < 1) return `${Math.max(1, Math.round(value * 24))} hours`;
		return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value)} days`;
	}

	function diskSummary(disk: DiskHealth | null): string {
		return disk
			? `${disk.name} · ${formatBytes(disk.available_bytes)} free`
			: 'Device not reported';
	}
</script>

<svelte:window onbeforeunload={protectReload} />

<form
	id="storage-settings-editor"
	class="scroll-mt-20 overflow-hidden rounded-md border border-hairline bg-surface"
	onsubmit={submit}
>
	<fieldset disabled={saving} class="contents">
		<header
			class="flex flex-wrap items-start justify-between gap-5 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">STORAGE CONFIGURATION</p>
				<h2 class="mt-1 text-xl font-semibold">Change recording storage</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">
					Choose where recordings live and how much space KeepPeek may use. Changes are staged until
					you apply the required restart.
				</p>
			</div>
			<ol class="flex min-w-[18rem] items-center gap-2 text-xs" aria-label="Storage setup steps">
				<li
					class="flex flex-1 items-center gap-2 border-b-2 pb-2 {step === 'configure'
						? 'border-primary text-text'
						: 'border-healthy text-text-muted'}"
					aria-current={step === 'configure' ? 'step' : undefined}
				>
					<span
						class="flex size-5 items-center justify-center rounded-full {step === 'review'
							? 'bg-healthy text-ground'
							: 'bg-primary text-on-primary'}">{step === 'review' ? '✓' : '1'}</span
					>
					Location & limits
				</li>
				<li
					class="flex flex-1 items-center gap-2 border-b-2 pb-2 {step === 'review'
						? 'border-primary text-text'
						: 'border-hairline text-text-faint'}"
					aria-current={step === 'review' ? 'step' : undefined}
				>
					<span class="flex size-5 items-center justify-center rounded-full border border-current"
						>2</span
					>
					Review
				</li>
			</ol>
		</header>

		{#if step === 'configure'}
			<div class="grid gap-0 lg:grid-cols-[minmax(0,1fr)_20rem]">
				<div class="space-y-6 border-b border-hairline p-5 lg:border-r lg:border-b-0">
					<section aria-labelledby="recording-location-heading">
						<div class="flex items-center gap-2">
							<HardDriveIcon class="size-4 text-primary-soft" />
							<h3 id="recording-location-heading" class="text-base font-semibold">
								Recording location
							</h3>
						</div>
						<p class="mt-1 text-xs leading-5 text-text-muted">
							KeepPeek derives active files, the catalog, and thumbnails from this location when
							they still use the standard layout.
						</p>
						<label class="mt-3 grid gap-1.5 text-sm font-medium" for="recording-location">
							Folder path
							<Input
								id="recording-location"
								value={draft.longTermPath}
								oninput={(event) => setRecordingLocation(event.currentTarget.value)}
								aria-invalid={fieldErrors.longTermPath !== null}
								aria-describedby={fieldErrors.longTermPath
									? 'recording-location-error'
									: 'recording-location-evidence'}
								autocomplete="off"
							/>
						</label>
						{#if fieldErrors.longTermPath}
							<p id="recording-location-error" class="mt-1 text-xs text-destructive">
								{fieldErrors.longTermPath}
							</p>
						{:else}
							<p
								id="recording-location-evidence"
								class="mt-2 flex items-center gap-2 text-xs text-text-muted"
							>
								<span class="size-1.5 rounded-full {destinationDisk ? 'bg-healthy' : 'bg-activity'}"
								></span>
								{diskSummary(destinationDisk)}
							</p>
						{/if}
						{#if suggestedDisks.length > 1}
							<div class="mt-3 flex flex-wrap gap-2" aria-label="Reported storage devices">
								{#each suggestedDisks as disk (disk.mount_point)}
									<Button
										type="button"
										variant="outline"
										size="sm"
										onclick={() => setRecordingLocation(disk.mount_point)}
									>
										{disk.name} · {formatBytes(disk.available_bytes)} free
									</Button>
								{/each}
							</div>
							<p class="mt-2 text-xs text-text-faint">
								Suggested devices are deduplicated persistent mounts with at least 16 GiB. Enter
								another path manually when needed.
							</p>
						{/if}
					</section>

					<section class="border-t border-hairline pt-5" aria-labelledby="archive-limit-heading">
						<div class="flex items-center gap-2">
							<DatabaseIcon class="size-4 text-primary-soft" />
							<h3 id="archive-limit-heading" class="text-base font-semibold">Archive limit</h3>
						</div>
						<div class="mt-3 grid gap-4 sm:grid-cols-[minmax(0,14rem)_1fr] sm:items-start">
							<label class="grid gap-1.5 text-sm font-medium" for="long-term-max-gigabytes">
								Maximum recording storage (GiB)
								<Input
									id="long-term-max-gigabytes"
									type="number"
									min="0"
									step="1"
									bind:value={draft.longTermMaxGigabytes}
									aria-invalid={fieldErrors.longTermMaxGigabytes !== null}
									aria-describedby="long-term-max-gigabytes-help long-term-max-gigabytes-error"
								/>
							</label>
							<div class="rounded-sm border border-hairline bg-raised px-4 py-3">
								<p class="font-mono text-2xs tracking-caps text-text-faint">PROJECTED RETENTION</p>
								<p class="mt-1 text-2xl font-semibold text-primary-soft">
									{formatRetention(proposedRetentionDays)}
								</p>
								<p class="mt-1 text-xs text-text-muted">
									{config.recording_estimate.known_streams} measured · {config.recording_estimate
										.unknown_streams} without bitrate evidence
								</p>
							</div>
						</div>
						<p id="long-term-max-gigabytes-help" class="mt-2 text-xs leading-5 text-text-muted">
							Oldest finalized recordings are pruned until the archive fits this cap. Zero means
							unlimited.
						</p>
						{#if fieldErrors.longTermMaxGigabytes}
							<p id="long-term-max-gigabytes-error" class="mt-1 text-xs text-destructive">
								{fieldErrors.longTermMaxGigabytes}
							</p>
						{/if}
						{#if estimatedPruneBytes > 0}
							<div
								class="mt-3 flex gap-2 rounded-sm border border-activity/50 bg-activity/5 p-3 text-xs leading-5 text-text-muted"
							>
								<TriangleAlertIcon class="mt-0.5 size-4 shrink-0 text-activity" />
								<span>
									This cap is below the indexed archive. After restart, KeepPeek may prune about
									{formatBytes(estimatedPruneBytes)} of the oldest footage.
								</span>
							</div>
						{/if}
						{#if String(draft.longTermMaxGigabytes).trim() === '0'}
							<p class="mt-2 text-xs text-text-muted">
								The archive cap is disabled; filesystem headroom limits still apply.
							</p>
						{/if}
						{#if allSafetyLimitsDisabled(draft)}
							<label
								class="mt-3 flex gap-2 rounded-sm border border-activity/50 bg-activity/5 p-3 text-sm"
							>
								<input
									type="checkbox"
									bind:checked={confirmUnlimited}
									class="mt-0.5 size-4 accent-primary"
								/>
								<span>I understand that unbounded storage can fill the recording disk.</span>
							</label>
						{/if}
					</section>

					<section
						class="border-t border-hairline pt-5"
						aria-labelledby="filesystem-safety-heading"
					>
						<div class="flex items-center gap-2">
							<TriangleAlertIcon class="size-4 text-primary-soft" />
							<h3 id="filesystem-safety-heading" class="text-base font-semibold">
								Filesystem safety
							</h3>
						</div>
						<p class="mt-1 text-xs leading-5 text-text-muted">
							Cleanup starts at the warning boundary and continues through the recovery target.
							Critical pressure pauses recording only when eligible footage cannot restore headroom.
						</p>
						<div class="mt-3 grid gap-4 lg:grid-cols-3 sm:grid-cols-2">
							{#each [{ field: 'minimumFreeGigabytes', id: 'minimum-free-gigabytes', label: 'Minimum free space (GiB)', minimum: 0, placeholder: undefined }, { field: 'maximumUsedPercent', id: 'maximum-used-percent', label: 'Maximum filesystem used (%)', minimum: 1, placeholder: 'Disabled' }, { field: 'warningFreeGigabytes', id: 'warning-free-gigabytes', label: 'Warning free space (GiB)', minimum: 0, placeholder: undefined }, { field: 'criticalFreeGigabytes', id: 'critical-free-gigabytes', label: 'Critical free space (GiB)', minimum: 0, placeholder: undefined }, { field: 'cleanupHysteresisGigabytes', id: 'cleanup-hysteresis-gigabytes', label: 'Cleanup hysteresis (GiB)', minimum: 0, placeholder: undefined }] as item (item.field)}
								<label class="grid gap-1.5 text-sm font-medium" for={item.id}>
									{item.label}
									<Input
										id={item.id}
										type="number"
										min={item.minimum}
										max={item.field === 'maximumUsedPercent' ? 99 : undefined}
										step="1"
										placeholder={item.placeholder}
										bind:value={draft[item.field as FieldName]}
										aria-invalid={fieldErrors[item.field as FieldName] !== null}
									/>
									{#if fieldErrors[item.field as FieldName]}
										<span class="text-xs text-destructive"
											>{fieldErrors[item.field as FieldName]}</span
										>
									{/if}
								</label>
							{/each}
						</div>
						{#if proposedPolicy}
							<div class="mt-4 grid gap-3 border-y border-hairline py-3 text-xs sm:grid-cols-3">
								<div>
									<span class="text-text-muted">Effective limit</span><strong
										class="mt-1 block font-mono"
										>{proposedPolicy.effectiveLimitBytes === null
											? 'Unlimited'
											: formatBytes(proposedPolicy.effectiveLimitBytes)}</strong
									>
								</div>
								<div>
									<span class="text-text-muted">Cleanup starts below</span><strong
										class="mt-1 block font-mono"
										>{formatBytes(proposedPolicy.warningFreeBytes)} free</strong
									>
								</div>
								<div>
									<span class="text-text-muted">Recovery target</span><strong
										class="mt-1 block font-mono"
										>{formatBytes(proposedPolicy.recoveryFreeBytes)} free</strong
									>
								</div>
							</div>
						{/if}
						{#if policyError}
							<p class="mt-2 text-xs text-destructive" role="alert">{policyError}</p>
						{/if}
					</section>

					{#if locationsChanged}
						<section class="border-t border-hairline pt-5" aria-labelledby="existing-files-heading">
							<div class="flex items-center gap-2">
								<MoveIcon class="size-4 text-primary-soft" />
								<h3 id="existing-files-heading" class="text-base font-semibold">Existing files</h3>
							</div>
							<div class="mt-3 grid gap-2">
								<label
									class="flex gap-3 rounded-sm border p-3 {migrationChoice === 'leave'
										? 'border-primary bg-primary/5'
										: 'border-hairline'}"
								>
									<input
										type="radio"
										name="migration-choice"
										value="leave"
										bind:group={migrationChoice}
										class="mt-0.5 size-4 accent-primary"
									/>
									<span>
										<span class="block text-sm font-medium">Use the new location from restart</span>
										<span class="mt-1 block text-xs leading-5 text-text-muted"
											>Leave current recordings in their existing location.</span
										>
									</span>
								</label>
								<label
									class="flex gap-3 rounded-sm border p-3 {migrationChoice === 'move'
										? 'border-primary bg-primary/5'
										: 'border-hairline'}"
								>
									<input
										type="radio"
										name="migration-choice"
										value="move"
										bind:group={migrationChoice}
										class="mt-0.5 size-4 accent-primary"
									/>
									<span>
										<span class="block text-sm font-medium"
											>Move existing storage during restart</span
										>
										<span class="mt-1 block text-xs leading-5 text-text-muted">
											{health?.storage.catalog?.fragment_bytes === undefined
												? 'Archive size unavailable.'
												: `${formatBytes(health.storage.catalog.fragment_bytes)} indexed archive.`}
											Cross-device moves copy before removing the source.
										</span>
									</span>
								</label>
							</div>
							{#if migrationError}
								<p class="mt-2 text-xs text-destructive" role="alert">{migrationError}</p>
							{/if}
						</section>
					{/if}

					<details class="group border-t border-hairline pt-5">
						<summary
							class="cursor-pointer text-sm font-semibold text-text marker:text-primary-soft"
						>
							<span class="flex items-center justify-between gap-3">
								<span>Advanced storage paths and writer controls</span>
								{#if advancedErrorCount > 0}
									<span id="advanced-storage-error-summary" class="text-xs text-destructive">
										{advancedErrorCount} advanced {advancedErrorCount === 1
											? 'setting needs'
											: 'settings need'} attention.
									</span>
								{/if}
							</span>
						</summary>
						<div class="mt-4 grid gap-4 sm:grid-cols-2">
							{#each [{ field: 'mediumTermPath', id: 'medium-term-path', label: 'Active recording path' }, { field: 'recordingCatalogPath', id: 'recording-catalog-path', label: 'Recording catalog path' }, { field: 'eventThumbnailPath', id: 'event-thumbnail-path', label: 'Event thumbnail path' }] as item (item.field)}
								<label class="grid gap-1.5 text-sm font-medium sm:col-span-2" for={item.id}>
									{item.label}
									<Input
										id={item.id}
										bind:value={draft[item.field as FieldName]}
										aria-invalid={fieldErrors[item.field as FieldName] !== null}
										aria-describedby={fieldErrors[item.field as FieldName]
											? `${item.id}-error`
											: undefined}
										autocomplete="off"
									/>
									{#if fieldErrors[item.field as FieldName]}
										<span id={`${item.id}-error`} class="text-xs text-destructive"
											>{fieldErrors[item.field as FieldName]}</span
										>
									{/if}
								</label>
							{/each}
							{#each [{ field: 'eventThumbnailMaxMegabytes', id: 'event-thumbnail-max-megabytes', label: 'Thumbnail storage limit (MiB)', minimum: 0 }, { field: 'shortTermSeconds', id: 'short-term-seconds', label: 'Memory buffer (seconds)', minimum: 0 }, { field: 'mediumTermSeconds', id: 'medium-term-seconds', label: 'Recording file duration (seconds)', minimum: 0 }, { field: 'flushIntervalSeconds', id: 'flush-interval-seconds', label: 'Flush interval (seconds)', minimum: 0 }, { field: 'writeBufferBytes', id: 'write-buffer-bytes', label: 'Write buffer (bytes)', minimum: 1 }] as item (item.field)}
								<label class="grid gap-1.5 text-sm font-medium" for={item.id}>
									{item.label}
									<Input
										id={item.id}
										type="number"
										min={item.minimum}
										step="1"
										bind:value={draft[item.field as FieldName]}
										aria-invalid={fieldErrors[item.field as FieldName] !== null}
									/>
									{#if fieldErrors[item.field as FieldName]}
										<span class="text-xs text-destructive"
											>{fieldErrors[item.field as FieldName]}</span
										>
									{/if}
								</label>
							{/each}
						</div>
					</details>
				</div>

				<aside class="space-y-5 bg-raised/55 p-5" aria-label="Storage change evidence">
					<div>
						<p class="font-mono text-2xs tracking-caps text-text-faint">CURRENT LOCATION</p>
						<p class="mt-2 font-mono text-xs leading-5 break-all">{initialDraft.longTermPath}</p>
						<p class="mt-1 text-xs text-text-muted">{diskSummary(sourceDisk)}</p>
					</div>
					<div class="border-t border-hairline pt-5">
						<p class="font-mono text-2xs tracking-caps text-text-faint">CURRENT ARCHIVE</p>
						<p class="mt-2 text-2xl font-semibold">
							{health?.storage.catalog?.fragment_bytes === undefined
								? 'Unavailable'
								: formatBytes(health.storage.catalog.fragment_bytes)}
						</p>
						<p class="mt-1 text-xs text-text-muted">Indexed finalized fragments</p>
					</div>
					<div class="border-t border-hairline pt-5">
						<p class="font-mono text-2xs tracking-caps text-text-faint">PROPOSED SAFETY POLICY</p>
						<p class="mt-2 text-sm font-medium">
							{proposedPolicy?.effectiveLimitBytes === null
								? 'Unlimited effective limit'
								: proposedPolicy
									? `${formatBytes(proposedPolicy.effectiveLimitBytes)} effective limit`
									: 'Complete valid limits'}
						</p>
						<p class="mt-1 text-xs leading-5 text-text-muted">
							Reserve {draft.minimumFreeGigabytes || '0'} GiB · Recovery {proposedPolicy
								? formatBytes(proposedPolicy.recoveryFreeBytes)
								: 'Unavailable'} free
						</p>
					</div>
					<div class="border-t border-hairline pt-5">
						<p class="font-mono text-2xs tracking-caps text-text-faint">BEFORE CONTINUING</p>
						<ul class="mt-3 space-y-2 text-xs leading-5 text-text-muted">
							<li class="flex gap-2">
								<CheckCircle2Icon class="mt-0.5 size-3.5 shrink-0 text-healthy" />
								Current configuration remains active until restart.
							</li>
							<li class="flex gap-2">
								<TriangleAlertIcon class="mt-0.5 size-3.5 shrink-0 text-activity" />
								KeepPeek validates path layout when settings are staged.
							</li>
							<li class="flex gap-2">
								<RefreshCwIcon class="mt-0.5 size-3.5 shrink-0 text-text-faint" />
								A restart is required for every storage change.
							</li>
						</ul>
					</div>
				</aside>
			</div>
		{:else}
			<section class="p-5" aria-labelledby="storage-review-heading">
				<div class="mx-auto max-w-4xl">
					<h3
						id="storage-review-heading"
						bind:this={reviewHeading}
						tabindex="-1"
						class="text-xl font-semibold outline-none"
					>
						Review storage changes
					</h3>
					<p class="mt-1 text-sm leading-6 text-text-muted">
						Settings are staged first. Nothing moves until you choose Apply changes and KeepPeek
						restarts.
					</p>

					<div class="mt-5 overflow-hidden rounded-sm border border-hairline">
						<div
							class="grid grid-cols-[minmax(8rem,0.7fr)_minmax(0,1fr)_minmax(0,1fr)] border-b border-hairline bg-raised px-4 py-2 font-mono text-2xs tracking-caps text-text-faint"
						>
							<span>SETTING</span><span>CURRENT</span><span>AFTER RESTART</span>
						</div>
						{#each changes as change (change.label)}
							<div
								class="grid grid-cols-[minmax(8rem,0.7fr)_minmax(0,1fr)_minmax(0,1fr)] gap-3 border-b border-hairline px-4 py-3 text-sm last:border-b-0"
							>
								<span class="font-medium">{change.label}</span>
								<span class="font-mono text-xs break-all text-text-muted">{change.from}</span>
								<span class="font-mono text-xs break-all">{change.to}</span>
							</div>
						{/each}
					</div>

					<div class="mt-5 grid gap-3 sm:grid-cols-2">
						<div class="rounded-sm border border-hairline bg-raised p-4">
							<p class="font-mono text-2xs tracking-caps text-text-faint">EXISTING FILES</p>
							<p class="mt-2 text-sm font-medium">
								{locationsChanged && migrationChoice === 'move'
									? 'Move during restart'
									: 'Leave in current location'}
							</p>
							<p class="mt-1 text-xs leading-5 text-text-muted">
								{locationsChanged && migrationChoice === 'move'
									? 'Same-device moves may rename; cross-device moves copy before source removal.'
									: 'Only new recordings use the proposed location after restart.'}
							</p>
						</div>
						<div class="rounded-sm border border-activity/45 bg-activity/5 p-4">
							<p class="font-mono text-2xs tracking-caps text-activity">RESTART REQUIRED</p>
							<p class="mt-2 text-sm font-medium">Stage now, apply when ready</p>
							<p class="mt-1 text-xs leading-5 text-text-muted">
								Recording restarts with the new paths and limits after Apply changes.
							</p>
						</div>
					</div>
					{#if proposedPolicy}
						<div class="mt-3 grid gap-3 border-y border-hairline py-4 text-sm sm:grid-cols-3">
							<div>
								<span class="text-xs text-text-muted">Effective limit</span><strong
									class="mt-1 block"
									>{proposedPolicy.effectiveLimitBytes === null
										? 'Unlimited'
										: formatBytes(proposedPolicy.effectiveLimitBytes)}</strong
								>
							</div>
							<div>
								<span class="text-xs text-text-muted">Warning boundary</span><strong
									class="mt-1 block">{formatBytes(proposedPolicy.warningFreeBytes)} free</strong
								>
							</div>
							<div>
								<span class="text-xs text-text-muted">Recovery target</span><strong
									class="mt-1 block">{formatBytes(proposedPolicy.recoveryFreeBytes)} free</strong
								>
							</div>
						</div>
					{/if}
					{#if estimatedPruneBytes > 0}
						<div class="mt-3 flex gap-3 rounded-sm border border-activity/50 bg-activity/5 p-4">
							<TriangleAlertIcon class="mt-0.5 size-4 shrink-0 text-activity" />
							<div>
								<p class="text-sm font-medium">The lower cap starts oldest-first pruning</p>
								<p class="mt-1 text-xs leading-5 text-text-muted">
									KeepPeek may remove about {formatBytes(estimatedPruneBytes)} of indexed footage after
									restart to fit the proposed limit.
								</p>
							</div>
						</div>
					{/if}
				</div>
			</section>
		{/if}
	</fieldset>

	{#if error}
		<p class="mx-5 mt-4 text-sm text-destructive" role="alert">{error}</p>
	{/if}

	<footer
		class="flex flex-wrap items-center justify-between gap-3 border-t border-hairline px-5 py-4"
	>
		<p class="text-xs text-text-muted" aria-live="polite">
			{dirty
				? `${changes.length} unsaved ${changes.length === 1 ? 'change' : 'changes'}`
				: 'No changes'}
		</p>
		<div class="flex items-center gap-2">
			<Button type="button" variant="outline" onclick={requestCancel} disabled={saving}
				>Cancel</Button
			>
			{#if step === 'review'}
				<Button
					type="button"
					variant="outline"
					onclick={() => (step = 'configure')}
					disabled={saving}
				>
					<ArrowLeftIcon /> Back
				</Button>
				<Button type="submit" disabled={saving || hasErrors || !dirty}>
					{#if saving}<RefreshCwIcon class="animate-spin" />{:else}<SaveIcon />{/if}
					{saving ? 'Staging changes' : 'Stage storage changes'}
				</Button>
			{:else}
				<Button type="submit" disabled={saving || hasErrors || !dirty}>
					Continue to review <ArrowRightIcon />
				</Button>
			{/if}
		</div>
	</footer>
</form>
