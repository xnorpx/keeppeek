<script lang="ts">
	import { onMount } from 'svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import * as Card from '$lib/components/ui/card/index.js';
	import { Separator } from '$lib/components/ui/separator/index.js';
	import {
		failureKindLabel,
		lifecycleStateLabel,
		type UsageReportingControlClient
	} from '$lib/control-client-usage-reporting';
	import type { UsageReportingPreview, UsageReportingState } from '$lib/proto/webrtc_pb';

	type Props = {
		controller: Pick<
			UsageReportingControlClient,
			'getState' | 'updateConsent' | 'preview' | 'resetIdentity' | 'deleteData'
		>;
	};

	let { controller }: Props = $props();
	const id = $props.id();

	let reportingState = $state.raw<UsageReportingState | null>(null);
	let preview = $state.raw<UsageReportingPreview | null>(null);
	let basic = $state(false);
	let cameraCompatibility = $state(false);
	let featureAdoption = $state(false);

	let loading = $state(true);
	let saving = $state(false);
	let previewLoading = $state(false);
	let resetting = $state(false);
	let deleting = $state(false);
	let mustReload = $state(false);
	let error = $state<string | null>(null);
	let statusMessage = $state<string | null>(null);
	let generation = 0;

	let disclosure = $derived(reportingState?.disclosure ?? null);
	let deliveryStatus = $derived(reportingState?.status ?? null);
	let renewalRequired = $derived(disclosure?.renewalRequired ?? false);
	let pendingDeletions = $derived(deliveryStatus?.pendingDeletionCount ?? 0);
	let lastFailureText = $derived(
		deliveryStatus ? failureKindLabel(deliveryStatus.lastFailure) : null
	);
	let lastDeletionFailureText = $derived(
		deliveryStatus ? failureKindLabel(deliveryStatus.lastDeletionFailure) : null
	);
	let lifecycleText = $derived(
		deliveryStatus ? lifecycleStateLabel(deliveryStatus.state) : 'Disabled'
	);
	let hasUnsavedChanges = $derived.by(() => {
		if (!reportingState?.consent) return false;
		return (
			basic !== reportingState.consent.basic ||
			cameraCompatibility !== reportingState.consent.cameraCompatibility ||
			featureAdoption !== reportingState.consent.featureAdoption
		);
	});
	let busy = $derived(loading || saving || resetting || deleting);

	onMount(() => {
		void loadState();
		return () => {
			generation += 1;
		};
	});

	function applyState(next: UsageReportingState): void {
		reportingState = next;
		basic = next.consent?.basic ?? false;
		cameraCompatibility = next.consent?.cameraCompatibility ?? false;
		featureAdoption = next.consent?.featureAdoption ?? false;
		mustReload = false;
	}

	function formatTimestamp(ms: bigint | undefined): string {
		if (ms === undefined || ms <= 0n) return 'Never';
		const asNumber = Number(ms);
		if (!Number.isFinite(asNumber)) return 'Never';
		return new Date(asNumber).toLocaleString();
	}

	async function loadState(): Promise<void> {
		const version = ++generation;
		loading = true;
		error = null;
		statusMessage = null;
		try {
			const loaded = await controller.getState();
			if (version === generation) {
				applyState(loaded);
			}
		} catch (cause) {
			if (version === generation) {
				error =
					cause instanceof Error
						? cause.message
						: 'Unable to load usage reporting settings. Reload to try again.';
			}
		} finally {
			if (version === generation) {
				loading = false;
			}
		}
	}

	function handleBasicChange(checked: boolean): void {
		basic = checked;
		if (!checked) {
			cameraCompatibility = false;
			featureAdoption = false;
		}
		preview = null;
	}

	async function refreshPreview(): Promise<void> {
		if (previewLoading) return;
		const version = generation;
		previewLoading = true;
		error = null;
		try {
			const nextPreview = await controller.preview({
				basic,
				cameraCompatibility,
				featureAdoption,
				disclosureRevision: disclosure?.currentDisclosureRevision ?? 0
			});
			if (version === generation) {
				preview = nextPreview;
				statusMessage = basic
					? 'Payload preview updated locally. No report was sent.'
					: 'All categories are disabled. No report will be sent.';
			}
		} catch (cause) {
			if (version === generation) {
				error = cause instanceof Error ? cause.message : 'Unable to build the payload preview.';
			}
		} finally {
			if (version === generation) {
				previewLoading = false;
			}
		}
	}

	async function saveConsent(acceptedDisclosureRevision: number, message: string): Promise<void> {
		if (!reportingState || busy || mustReload) return;
		const version = generation;
		saving = true;
		error = null;
		statusMessage = null;
		try {
			const updated = await controller.updateConsent(
				{
					basic,
					cameraCompatibility,
					featureAdoption,
					acceptedDisclosureRevision
				},
				reportingState.configurationRevision
			);
			if (version !== generation) return;
			applyState(updated);
			preview = null;
			statusMessage = message;
		} catch (cause) {
			if (version !== generation) return;
			mustReload = true;
			error =
				cause instanceof Error
					? cause.message
					: 'Usage reporting settings could not be saved. Reload and try again.';
		} finally {
			if (version === generation) {
				saving = false;
			}
		}
	}

	async function handleSubmit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		const priorRevision = reportingState?.consent?.consentedDisclosureRevision ?? 0;
		const preservePriorConsent = Boolean(basic && disclosure?.renewalRequired);
		const targetRevision = !basic
			? priorRevision
			: preservePriorConsent
				? priorRevision
				: (disclosure?.currentDisclosureRevision ?? 1);
		await saveConsent(
			targetRevision,
			!basic
				? 'Usage reporting disabled. Future reports are stopped and deletion was requested.'
				: preservePriorConsent
					? 'Usage reporting preferences saved under prior consent. Newly added fields remain disabled until you accept the updated disclosure.'
					: 'Usage reporting preferences saved.'
		);
	}

	async function handleAcceptRenewal(): Promise<void> {
		const targetRevision = disclosure?.currentDisclosureRevision ?? 1;
		await saveConsent(targetRevision, 'Updated usage reporting disclosure accepted.');
	}

	async function handleKeepPriorConsent(): Promise<void> {
		const priorRevision = reportingState?.consent?.consentedDisclosureRevision ?? 0;
		await saveConsent(
			priorRevision,
			'Kept prior consent only. Newly added fields remain disabled.'
		);
	}

	async function handleResetIdentity(): Promise<void> {
		if (!reportingState || busy || mustReload) return;
		const version = generation;
		resetting = true;
		error = null;
		statusMessage = null;
		try {
			const updated = await controller.resetIdentity(reportingState.configurationRevision);
			if (version !== generation) return;
			applyState(updated);
			preview = null;
			statusMessage =
				'Installation identity rotated and deletion of the previous receiver row was requested.';
		} catch (cause) {
			if (version !== generation) return;
			mustReload = true;
			error =
				cause instanceof Error
					? cause.message
					: 'Unable to reset installation identity. Reload and try again.';
		} finally {
			if (version === generation) {
				resetting = false;
			}
		}
	}

	async function handleRetryDeletion(): Promise<void> {
		if (!reportingState || busy || mustReload) return;
		const version = generation;
		deleting = true;
		error = null;
		statusMessage = null;
		try {
			const updated = await controller.deleteData(reportingState.configurationRevision);
			if (version !== generation) return;
			applyState(updated);
			statusMessage =
				updated.status?.pendingDeletionCount === 0
					? 'Stored receiver rows were deleted.'
					: 'Deletion attempt finished. Some deletions are still pending.';
		} catch (cause) {
			if (version !== generation) return;
			mustReload = true;
			error =
				cause instanceof Error ? cause.message : 'Unable to retry deletion. Reload and try again.';
		} finally {
			if (version === generation) {
				deleting = false;
			}
		}
	}
</script>

<section id="usage-reporting" class="scroll-mt-4" aria-labelledby="{id}-heading">
	<Card.Root>
		<Card.Header>
			<Card.Title>
				<h2 id="{id}-heading">Usage reporting</h2>
			</Card.Title>
			<Card.Description>
				Optional, Administrator-controlled sharing of aggregate system, camera compatibility, and
				feature adoption data. Off by default.
			</Card.Description>
		</Card.Header>
		<Card.Content class="space-y-5">
			{#if loading}
				<p class="text-sm text-muted-foreground">Loading usage reporting settings…</p>
			{:else if reportingState && disclosure && deliveryStatus}
				<div class="space-y-2 rounded-md border border-border bg-muted/30 p-3 text-sm">
					<p>
						<strong>Purpose:</strong> Help prioritize OS, CPU, camera model, codec, and feature support
						using documented allowlisted fields only.
					</p>
					<p>
						<strong>Privacy &amp; retention:</strong> Raw per-installation rows use a dedicated
						random telemetry UUID and are <strong>pseudonymous</strong>, not anonymous. Rows expire
						after at most <strong>{disclosure.maxRetentionDays} days</strong>, source IPs are not
						persisted, uncompressed payloads are capped at
						<strong>{Math.round(disclosure.maxPayloadBytes / 1024)} KiB</strong>, and public outputs
						publish only aggregates representing at least
						<strong>{disclosure.aggregateMinInstallations} installations</strong> per cell.
					</p>
					<p>
						<strong>Receiver:</strong>
						<span class="font-mono text-xs">
							{disclosure.receiverOrigin || 'Not configured (local preview only)'}
						</span>
					</p>
					<div class="flex flex-wrap gap-3 pt-1 text-xs">
						<a
							href={disclosure.schemaUrl}
							target="_blank"
							rel="noreferrer"
							class="underline underline-offset-4">Published schema (v{disclosure.schemaVersion})</a
						>
						<a
							href={disclosure.documentationUrl}
							target="_blank"
							rel="noreferrer"
							class="underline underline-offset-4">Documentation</a
						>
						<a
							href={disclosure.receiverSourceUrl}
							target="_blank"
							rel="noreferrer"
							class="underline underline-offset-4">Receiver source</a
						>
						<a
							href={disclosure.aggregateDataUrl}
							target="_blank"
							rel="noreferrer"
							class="underline underline-offset-4">Public aggregate dataset</a
						>
					</div>
				</div>

				{#if renewalRequired}
					<div
						class="space-y-2 rounded-md border border-amber-500/50 bg-amber-500/10 p-3 text-sm"
						role="region"
						aria-label="Updated disclosure requires renewed consent"
					>
						<p class="font-medium">
							Updated usage reporting disclosure (revision {disclosure.currentDisclosureRevision})
						</p>
						<p class="text-muted-foreground">
							New or more detailed fields remain disabled until an Administrator accepts the updated
							disclosure. Previously approved unchanged fields continue as configured.
						</p>
						{#if disclosure.pendingRenewalFields.length > 0}
							<p class="font-mono text-xs">
								Pending renewal: {disclosure.pendingRenewalFields.join(', ')}
							</p>
						{/if}
						<div class="flex flex-wrap gap-2 pt-1">
							<Button
								type="button"
								size="sm"
								disabled={busy || mustReload}
								onclick={() => void handleAcceptRenewal()}
							>
								Accept updated disclosure
							</Button>
							<Button
								type="button"
								variant="outline"
								size="sm"
								disabled={busy || mustReload}
								onclick={() => void handleKeepPriorConsent()}
							>
								Keep existing consent only
							</Button>
						</div>
					</div>
				{/if}

				<form class="space-y-4" onsubmit={handleSubmit}>
					<fieldset disabled={busy || mustReload} class="space-y-3">
						<legend class="text-sm font-medium">Reporting categories</legend>
						<label
							for="{id}-basic"
							class="flex cursor-pointer items-start gap-3 rounded-md border border-border p-3"
						>
							<input
								id="{id}-basic"
								type="checkbox"
								class="mt-1 size-4"
								checked={basic}
								onchange={(event) => handleBasicChange(event.currentTarget.checked)}
							/>
							<span class="space-y-0.5 text-sm">
								<span class="block font-medium">Basic system data</span>
								<span class="block text-muted-foreground">
									Dedicated telemetry UUID, KeepPeek version and channel, deployment type, OS family
									and normalized version, architecture, CPU vendor/family, core counts, memory
									bucket, and video-acceleration backends. Required before enabling additional
									categories.
								</span>
							</span>
						</label>

						<label
							for="{id}-camera"
							class="flex items-start gap-3 rounded-md border border-border p-3 {basic
								? 'cursor-pointer'
								: 'cursor-not-allowed opacity-60'}"
						>
							<input
								id="{id}-camera"
								type="checkbox"
								class="mt-1 size-4"
								checked={cameraCompatibility}
								disabled={!basic}
								onchange={(event) => {
									cameraCompatibility = event.currentTarget.checked;
									preview = null;
								}}
							/>
							<span class="space-y-0.5 text-sm">
								<span class="block font-medium">Camera compatibility data</span>
								<span class="block text-muted-foreground">
									Configured, connected, decodable, and recording camera counts grouped by
									normalized manufacturer/model, backend, transport, video codec, resolution bucket,
									and bounded capability IDs. Never includes camera names, IPs, URLs, serials,
									firmware, or credentials.
								</span>
							</span>
						</label>

						<label
							for="{id}-feature"
							class="flex items-start gap-3 rounded-md border border-border p-3 {basic
								? 'cursor-pointer'
								: 'cursor-not-allowed opacity-60'}"
						>
							<input
								id="{id}-feature"
								type="checkbox"
								class="mt-1 size-4"
								checked={featureAdoption}
								disabled={!basic}
								onchange={(event) => {
									featureAdoption = event.currentTarget.checked;
									preview = null;
								}}
							/>
							<span class="space-y-0.5 text-sm">
								<span class="block font-medium">Feature adoption data</span>
								<span class="block text-muted-foreground">
									Camera counts by recording mode, storage-capacity and projected-retention buckets,
									and bounded identifiers for enabled integrations and event-source types without
									endpoints or account details.
								</span>
							</span>
						</label>
					</fieldset>

					<div class="flex flex-wrap items-center gap-2">
						<Button type="submit" disabled={busy || mustReload || !hasUnsavedChanges}>
							{saving ? 'Saving…' : 'Save usage reporting preferences'}
						</Button>
						<Button
							type="button"
							variant="outline"
							disabled={previewLoading}
							onclick={() => void refreshPreview()}
						>
							{previewLoading ? 'Building preview…' : 'Preview exact JSON payload'}
						</Button>
						{#if mustReload}
							<Button type="button" variant="outline" onclick={() => void loadState()}>
								Reload settings
							</Button>
						{/if}
					</div>
				</form>

				{#if preview}
					<div class="space-y-2 rounded-md border border-border p-3">
						<div class="flex flex-wrap items-center justify-between gap-2 text-xs">
							<span class="font-medium">Exact serialized payload preview</span>
							<span class="font-mono text-muted-foreground">
								{preview.payloadBytes} / {preview.maxPayloadBytes} bytes
							</span>
						</div>
						{#if !basic}
							<p class="text-xs text-muted-foreground">
								All categories are disabled. No JSON payload is built or sent.
							</p>
						{:else if !preview.valid}
							<p class="text-xs text-destructive" role="alert">
								Preview rejected locally: {failureKindLabel(preview.validationError) ??
									'Validation failed'}.
							</p>
						{:else}
							{#if preview.usesPlaceholderIdentity}
								<p class="text-xs text-muted-foreground">
									Uses a non-persisted placeholder UUID until you save opt-in consent. Refreshing
									preview never sends a report.
								</p>
							{/if}
							<pre
								class="max-h-72 overflow-x-auto rounded bg-muted p-2.5 font-mono text-xs"
								data-testid="usage-reporting-preview-json">{preview.payloadJson}</pre
							>
						{/if}
					</div>
				{/if}

				<Separator />

				<div class="space-y-3">
					<h3 class="text-sm font-medium">Delivery &amp; deletion status</h3>
					<dl class="grid gap-2 text-sm sm:grid-cols-2">
						<div class="flex justify-between gap-2 rounded border border-border px-3 py-2">
							<dt class="text-muted-foreground">State</dt>
							<dd class="font-medium" data-testid="usage-reporting-lifecycle">{lifecycleText}</dd>
						</div>
						<div class="flex justify-between gap-2 rounded border border-border px-3 py-2">
							<dt class="text-muted-foreground">Installation ID</dt>
							<dd>{deliveryStatus.identityConfigured ? 'Generated' : 'None'}</dd>
						</div>
						<div class="flex justify-between gap-2 rounded border border-border px-3 py-2">
							<dt class="text-muted-foreground">Last attempt</dt>
							<dd>{formatTimestamp(deliveryStatus.lastAttemptAtMs)}</dd>
						</div>
						<div class="flex justify-between gap-2 rounded border border-border px-3 py-2">
							<dt class="text-muted-foreground">Last success</dt>
							<dd>{formatTimestamp(deliveryStatus.lastSuccessAtMs)}</dd>
						</div>
						<div class="flex justify-between gap-2 rounded border border-border px-3 py-2">
							<dt class="text-muted-foreground">Next scheduled</dt>
							<dd>{formatTimestamp(deliveryStatus.nextScheduledAtMs)}</dd>
						</div>
						<div class="flex justify-between gap-2 rounded border border-border px-3 py-2">
							<dt class="text-muted-foreground">Pending deletions</dt>
							<dd data-testid="usage-reporting-pending-deletions">{pendingDeletions}</dd>
						</div>
					</dl>
					{#if lastFailureText}
						<p class="text-xs text-destructive" role="status">
							Last delivery issue: {lastFailureText}
						</p>
					{/if}
					{#if lastDeletionFailureText}
						<p class="text-xs text-destructive" role="status">
							Last deletion issue: {lastDeletionFailureText}
						</p>
					{/if}

					<div class="flex flex-wrap gap-2">
						<Button
							type="button"
							variant="outline"
							size="sm"
							disabled={busy || mustReload || !deliveryStatus.identityConfigured}
							onclick={() => void handleResetIdentity()}
						>
							{resetting ? 'Resetting…' : 'Reset installation ID'}
						</Button>
						<Button
							type="button"
							variant="outline"
							size="sm"
							disabled={busy || mustReload || pendingDeletions === 0}
							onclick={() => void handleRetryDeletion()}
						>
							{deleting ? 'Retrying deletion…' : 'Retry deletion'}
						</Button>
					</div>
				</div>
			{/if}

			{#if statusMessage}
				<p class="text-sm text-muted-foreground" role="status" aria-live="polite">
					{statusMessage}
				</p>
			{/if}
			{#if error}
				<p class="text-sm text-destructive" role="alert">{error}</p>
			{/if}
		</Card.Content>
	</Card.Root>
</section>
