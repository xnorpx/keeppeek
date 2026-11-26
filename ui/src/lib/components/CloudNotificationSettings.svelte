<script lang="ts">
	import { onMount } from 'svelte';
	import { useCapabilityState } from '$lib/capability-context';
	import { useControlClient } from '$lib/control-context';
	import { Button } from '$lib/components/ui/button/index.js';
	import {
		CLOUD_NOTIFICATIONS_CAPABILITY_ID,
		type CloudNotificationFailureCode,
		type CloudNotificationSettings,
		type CloudNotificationState,
		type CloudNotificationStatus,
		type CloudNotificationTestResult
	} from '$lib/control-client-cloud-notifications';
	import BellRingIcon from '@lucide/svelte/icons/bell-ring';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import FlaskConicalIcon from '@lucide/svelte/icons/flask-conical';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import ShieldCheckIcon from '@lucide/svelte/icons/shield-check';

	const capabilities = useCapabilityState();
	const controlClient = useControlClient();

	let supported = $derived(capabilities.supports(CLOUD_NOTIFICATIONS_CAPABILITY_ID));
	let settings = $state<CloudNotificationSettings | null>(null);
	let status = $state<CloudNotificationStatus | null>(null);
	let draftEnabled = $state(false);
	let draftPreviews = $state(false);
	let loading = $state(false);
	let saving = $state(false);
	let testing = $state(false);
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let lastTest = $state<CloudNotificationTestResult | null>(null);

	let canEnablePreviews = $derived(draftEnabled && (status?.paired ?? false));
	let dirty = $derived(
		settings !== null &&
			(draftEnabled !== settings.enabled || draftPreviews !== settings.imagePreviewsEnabled)
	);

	const stateLabels: Record<CloudNotificationState, string> = {
		disabled: 'DISABLED',
		unpaired: 'UNPAIRED',
		ready: 'READY',
		throttled: 'THROTTLED',
		backing_off: 'BACKING OFF',
		unavailable: 'UNAVAILABLE'
	};

	const failureLabels: Record<CloudNotificationFailureCode, string> = {
		none: 'None',
		local_limit: 'Local pacing limit',
		service_limit: 'Keeppeek Service limit',
		dependency: 'Service dependency error',
		credential_rejected: 'Pairing credential rejected (repair required)',
		integrity: 'Signal integrity conflict',
		expired: 'Pending signal expired (5-minute TTL)'
	};

	onMount(() => {
		if (supported) {
			void reload();
		}
	});

	async function reload(): Promise<void> {
		if (!supported || loading) return;
		loading = true;
		try {
			const [nextSettings, nextStatus] = await Promise.all([
				controlClient.getCloudNotificationSettings(),
				controlClient.getCloudNotificationStatus()
			]);
			settings = nextSettings;
			status = nextStatus;
			draftEnabled = nextSettings.enabled;
			draftPreviews = nextSettings.imagePreviewsEnabled;
			error = null;
		} catch (cause) {
			error = errorMessage(cause, 'Unable to load Keeppeek Service push settings.');
		} finally {
			loading = false;
		}
	}

	async function save(): Promise<void> {
		if (!settings || saving) return;
		saving = true;
		notice = null;
		try {
			const updated = await controlClient.updateCloudNotificationSettings(
				settings.revision,
				draftEnabled,
				draftEnabled ? draftPreviews : false
			);
			settings = updated;
			draftEnabled = updated.enabled;
			draftPreviews = updated.imagePreviewsEnabled;
			status = await controlClient.getCloudNotificationStatus();
			error = null;
			notice = 'Cloud signaling settings saved on this NVR.';
		} catch (cause) {
			const text = errorMessage(cause, 'Unable to save cloud signaling settings.');
			error = text;
			if (text.toLowerCase().includes('conflict')) {
				await reload();
			}
		} finally {
			saving = false;
		}
	}

	async function sendTest(): Promise<void> {
		if (testing || !settings?.enabled || !status?.paired) return;
		testing = true;
		notice = null;
		try {
			const result = await controlClient.testCloudNotification();
			lastTest = result;
			status = await controlClient.getCloudNotificationStatus();
			error = null;
			notice = result.accepted
				? 'Keeppeek Service accepted the compact test signal. Acceptance does not confirm mobile device display.'
				: `Test signal was not accepted (${failureLabels[result.failure]}).`;
		} catch (cause) {
			error = errorMessage(cause, 'Unable to send a cloud test signal.');
		} finally {
			testing = false;
		}
	}

	function toggleEnabled(checked: boolean): void {
		draftEnabled = checked;
		if (!checked) {
			draftPreviews = false;
		}
	}

	function formatTime(ms: number | null): string {
		if (ms === null) return '—';
		return new Date(ms).toLocaleTimeString([], {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	function errorMessage(cause: unknown, fallback: string): string {
		return cause instanceof Error && cause.message.trim().length > 0 ? cause.message : fallback;
	}
</script>

<section aria-labelledby="cloud-push-heading" class="rounded-md border border-hairline bg-surface">
	<header
		class="flex flex-wrap items-start justify-between gap-4 border-b border-hairline px-5 py-4"
	>
		<div class="max-w-2xl space-y-1">
			<div class="flex items-center gap-2">
				<BellRingIcon class="size-4 text-primary-soft" />
				<p class="font-mono text-2xs tracking-caps text-primary-soft">
					KEEPPEEK SERVICE · MOBILE PUSH SIGNALING
				</p>
			</div>
			<h3 id="cloud-push-heading" class="text-base font-semibold">
				Bounded mobile push signaling
			</h3>
			<p class="text-xs leading-5 text-text-muted">
				Emits a compact 109-byte signed signal only after local rule matching and event persistence.
				Keeppeek Service never receives camera names, event details, media URLs, or image bytes.
				Signal acceptance confirms service admission only, not provider delivery or phone display.
			</p>
		</div>
		{#if supported}
			<div class="flex flex-wrap items-center gap-2">
				{#if status}
					<span
						class="rounded-sm border border-hairline-strong bg-raised px-2.5 py-1 font-mono text-2xs tracking-caps"
						data-testid="cloud-push-state"
					>
						{stateLabels[status.state]}
					</span>
				{/if}
				<Button
					type="button"
					variant="outline"
					size="sm"
					disabled={loading}
					onclick={() => void reload()}
				>
					<RefreshCwIcon class="size-3.5" />
					Refresh cloud status
				</Button>
			</div>
		{/if}
	</header>

	{#if !supported}
		<div
			class="px-5 py-4 text-xs text-text-muted"
			role="status"
			data-testid="cloud-push-unavailable"
		>
			Keeppeek Service mobile push signaling is unavailable because this server does not advertise
			<code class="font-mono">{CLOUD_NOTIFICATIONS_CAPABILITY_ID}</code>. Local recording,
			notification rules, Pushover, browser alerts, and MQTT remain independent.
		</div>
	{:else}
		<div class="grid gap-5 p-5 lg:grid-cols-[1.1fr_0.9fr]">
			<div class="space-y-4">
				{#if error}
					<div
						class="flex items-start gap-2 rounded-sm border border-destructive/50 bg-destructive/10 px-3 py-2.5 text-xs text-destructive"
						role="alert"
					>
						<CircleAlertIcon class="mt-0.5 size-4 shrink-0" />
						<span>{error}</span>
					</div>
				{/if}
				{#if notice}
					<div
						class="flex items-start gap-2 rounded-sm border border-hairline-strong bg-raised px-3 py-2.5 text-xs text-text"
						role="status"
					>
						<ShieldCheckIcon class="mt-0.5 size-4 shrink-0 text-primary-soft" />
						<span>{notice}</span>
					</div>
				{/if}

				<div class="space-y-3 rounded-sm border border-hairline bg-raised/30 p-4">
					<label class="flex items-start gap-3 text-sm">
						<input
							type="checkbox"
							class="mt-0.5 size-4 rounded border-hairline-strong"
							checked={draftEnabled}
							disabled={loading || saving || settings === null}
							onchange={(event) => toggleEnabled(event.currentTarget.checked)}
						/>
						<span>
							<span class="block font-medium">Enable Keeppeek Service signaling</span>
							<span class="mt-0.5 block text-xs text-text-muted">
								Defaults off. Bounded to 1 pending signal, 30s spacing, 120/hour, and 500/day.
							</span>
						</span>
					</label>

					<label class="flex items-start gap-3 text-sm">
						<input
							type="checkbox"
							class="mt-0.5 size-4 rounded border-hairline-strong"
							checked={draftPreviews}
							disabled={loading || saving || !canEnablePreviews}
							onchange={(event) => {
								draftPreviews = event.currentTarget.checked;
							}}
						/>
						<span>
							<span class="block font-medium">Allow private direct still-image previews</span>
							<span class="mt-0.5 block text-xs text-text-muted">
								Defaults off and stays authoritative on this NVR. Paired mobile devices fetch
								canonical still images directly from this private NVR; video, animated images, and
								audio are unsupported.
							</span>
						</span>
					</label>

					<div class="flex flex-wrap items-center gap-2 pt-2">
						<Button
							type="button"
							size="sm"
							disabled={loading || saving || !dirty}
							onclick={() => void save()}
						>
							{saving ? 'Saving…' : 'Save cloud settings'}
						</Button>
						<Button
							type="button"
							variant="outline"
							size="sm"
							disabled={loading || testing || !settings?.enabled || !status?.paired}
							onclick={() => void sendTest()}
						>
							<FlaskConicalIcon class="size-3.5" />
							{testing ? 'Sending test…' : 'Send test signal (3/hr limit)'}
						</Button>
					</div>
				</div>

				{#if lastTest}
					<p class="font-mono text-2xs text-text-muted" data-testid="cloud-push-test-summary">
						Last test: {lastTest.accepted
							? 'accepted by Keeppeek Service'
							: failureLabels[lastTest.failure]}
						{#if lastTest.nextEligibleAtMs !== null}
							· Next eligible {formatTime(lastTest.nextEligibleAtMs)}
						{/if}
					</p>
				{/if}
			</div>

			<div class="space-y-3 rounded-sm border border-hairline bg-raised/20 p-4">
				<h4 class="font-mono text-2xs tracking-caps text-text-faint"
					>BOUNDED HEALTH &amp; SUPPRESSION</h4
				>
				<dl class="divide-y divide-hairline text-xs">
					<div class="flex justify-between gap-3 py-2">
						<dt class="text-text-muted">Pairing status</dt>
						<dd class="font-mono">{status?.paired ? 'Paired' : 'Unpaired'}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2">
						<dt class="text-text-muted">Pending signal slot</dt>
						<dd class="font-mono">{status?.pending ? '1 pending (newest only)' : 'Idle'}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2">
						<dt class="text-text-muted">Accepted (1h / 24h)</dt>
						<dd class="font-mono">
							{status ? `${status.acceptedLastHour} / ${status.acceptedLastDay}` : '—'}
						</dd>
					</div>
					<div class="flex justify-between gap-3 py-2">
						<dt class="text-text-muted">Suppressed or replaced (1h / 24h)</dt>
						<dd class="font-mono">
							{status ? `${status.suppressedLastHour} / ${status.suppressedLastDay}` : '—'}
						</dd>
					</div>
					<div class="flex justify-between gap-3 py-2">
						<dt class="text-text-muted">Last service acceptance</dt>
						<dd class="font-mono">{formatTime(status?.lastAcceptedAtMs ?? null)}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2">
						<dt class="text-text-muted">Next eligible window</dt>
						<dd class="font-mono">{formatTime(status?.nextEligibleAtMs ?? null)}</dd>
					</div>
					<div class="flex justify-between gap-3 py-2">
						<dt class="text-text-muted">Last reason</dt>
						<dd class="font-mono">{status ? failureLabels[status.lastFailure] : '—'}</dd>
					</div>
				</dl>
			</div>
		</div>
	{/if}
</section>
