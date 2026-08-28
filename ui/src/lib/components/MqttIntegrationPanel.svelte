<script lang="ts">
	import { useCapabilityState } from '$lib/capability-context';
	import { useControlClient } from '$lib/control-context';
	import type { MqttIntegration, MqttSettingsUpdate } from '$lib/integrations';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import CheckIcon from '@lucide/svelte/icons/check';
	import LoaderCircleIcon from '@lucide/svelte/icons/loader-circle';
	import PlugZapIcon from '@lucide/svelte/icons/plug-zap';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SaveIcon from '@lucide/svelte/icons/save';

	type Draft = {
		enabled: boolean;
		brokerUrl: string;
		clientId: string;
		instanceId: string;
		forwarderId: string;
		topicPrefix: string;
		username: string;
		password: string;
		clearPassword: boolean;
		tlsCaPath: string;
		qos: string;
		retainEvents: boolean;
		retainHealth: boolean;
		outboxMaxMb: string;
		retryMinMs: string;
		retryMaxMs: string;
	};

	const controlClient = useControlClient();
	const capabilities = useCapabilityState();
	let mqttAvailable = $derived(capabilities.supports('keeppeek.mqtt-forwarder.v1'));
	let runtimeStarted = $state(false);
	let integration = $state.raw<MqttIntegration | null>(null);
	let draft = $state<Draft | null>(null);
	let loading = $state(true);
	let saving = $state(false);
	let testing = $state(false);
	let editing = $state(false);
	let error = $state<string | null>(null);
	let result = $state<string | null>(null);

	$effect(() => {
		if (!mqttAvailable) {
			loading = false;
			integration = null;
			draft = null;
			editing = false;
			runtimeStarted = false;
			return;
		}
		if (!runtimeStarted) {
			runtimeStarted = true;
			loading = true;
			void load(true);
		}
		const interval = window.setInterval(() => void load(false), 5_000);
		return () => window.clearInterval(interval);
	});

	function draftFrom(integration: MqttIntegration): Draft {
		const config = integration.configuration;
		return {
			enabled: config.enabled,
			brokerUrl: config.broker_url,
			clientId: config.client_id,
			instanceId: config.instance_id,
			forwarderId: config.forwarder_id,
			topicPrefix: config.topic_prefix,
			username: config.username ?? '',
			password: '',
			clearPassword: false,
			tlsCaPath: config.tls_ca_path ?? '',
			qos: config.qos.toString(),
			retainEvents: config.retain_events,
			retainHealth: config.retain_health,
			outboxMaxMb: config.outbox_max_mb.toString(),
			retryMinMs: config.retry_min_ms.toString(),
			retryMaxMs: config.retry_max_ms.toString()
		};
	}

	async function load(initializeDraft: boolean): Promise<void> {
		try {
			const next = await controlClient.getMqttIntegration();
			integration = next;
			if (initializeDraft || draft === null) draft = draftFrom(next);
			error = null;
		} catch (cause) {
			if (initializeDraft) {
				error = cause instanceof Error ? cause.message : 'MQTT integration is unavailable.';
			}
		} finally {
			if (initializeDraft) loading = false;
		}
	}

	function wholeNumber(value: string, label: string, minimum: number, maximum: number): number {
		if (!/^\d+$/.test(value.trim())) throw new Error(`${label} must be a whole number.`);
		const parsed = Number(value);
		if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
			throw new Error(`${label} must be between ${minimum} and ${maximum}.`);
		}
		return parsed;
	}

	function updateFromDraft(): MqttSettingsUpdate {
		if (!draft || !integration) throw new Error('MQTT settings are unavailable.');
		const update: MqttSettingsUpdate = {
			enabled: draft.enabled,
			broker_url: draft.brokerUrl.trim(),
			client_id: draft.clientId.trim(),
			instance_id: draft.instanceId.trim(),
			forwarder_id: draft.forwarderId.trim(),
			topic_prefix: draft.topicPrefix.trim(),
			username: draft.username.trim() || null,
			tls_ca_path: draft.tlsCaPath.trim() || null,
			qos: wholeNumber(draft.qos, 'QoS', 0, 2),
			retain_events: draft.retainEvents,
			retain_health: draft.retainHealth,
			outbox_max_mb: wholeNumber(draft.outboxMaxMb, 'Outbox limit', 1, 65_536),
			retry_min_ms: wholeNumber(draft.retryMinMs, 'Retry minimum', 1, 3_600_000),
			retry_max_ms: wholeNumber(draft.retryMaxMs, 'Retry maximum', 1, 3_600_000),
			expected_configuration_revision: integration.configuration_revision
		};
		if (draft.password) update.password = draft.password;
		if (draft.clearPassword) update.clear_password = true;
		return update;
	}

	async function save(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (saving || testing) return;
		saving = true;
		error = null;
		result = null;
		try {
			const updated = await controlClient.updateMqttIntegration(updateFromDraft());
			integration = updated;
			draft = draftFrom(updated);
			editing = false;
			result = updated.configuration.enabled
				? 'MQTT settings saved and applied.'
				: 'MQTT forwarding disabled. Queued events remain durable.';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'MQTT settings were not saved.';
		} finally {
			saving = false;
		}
	}

	async function testConnection(): Promise<void> {
		if (saving || testing) return;
		testing = true;
		error = null;
		result = null;
		try {
			const test = await controlClient.testMqttIntegration(updateFromDraft());
			if (!test.ok) throw new Error(test.detail);
			result = test.detail;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'MQTT connection test failed.';
		} finally {
			testing = false;
		}
	}

	function timestamp(value: number | null): string {
		return value === null ? 'Never' : new Date(value).toLocaleString();
	}

	function bytes(value: number): string {
		if (value < 1_024) return `${value} B`;
		if (value < 1_048_576) return `${(value / 1_024).toFixed(1)} KiB`;
		return `${(value / 1_048_576).toFixed(1)} MiB`;
	}

	function beginEditing(): void {
		if (!integration) return;
		draft = draftFrom(integration);
		error = null;
		result = null;
		editing = true;
	}

	function cancelEditing(): void {
		if (integration) draft = draftFrom(integration);
		error = null;
		editing = false;
	}
</script>

<article class="overflow-hidden" aria-labelledby="mqtt-integration-heading" data-mqtt-integration>
	{#if !mqttAvailable}
		<div class="flex min-h-[236px] flex-col lg:flex-row">
			<div
				class="flex flex-col gap-2.5 border-b border-hairline p-5 lg:w-[400px] lg:shrink-0 lg:border-r lg:border-b-0"
			>
				<div class="flex items-center gap-2.5">
					<span class="size-2 shrink-0 rounded-full bg-text-faint"></span>
					<h3 id="mqtt-integration-heading" class="text-lg leading-[22px] font-semibold">MQTT 5</h3>
				</div>
				<p class="font-mono text-2xs tracking-caps text-text-faint">RUNTIME UNAVAILABLE</p>
				<p class="text-[13px] leading-[21px] text-text-muted">
					This server does not advertise the MQTT 5 forwarder contract. No broker state is inferred
					from generated types.
				</p>
				<Button type="button" variant="outline" size="sm" disabled>Configuration unavailable</Button
				>
			</div>
			<div class="flex min-w-0 flex-1 items-center p-5">
				<div class="w-full rounded-sm border border-activity/45 bg-activity/5 px-3 py-3">
					<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-activity">
						Server update required
					</div>
					<p class="mt-2 font-mono text-xs text-text-muted">keeppeek.mqtt-forwarder.v1</p>
				</div>
			</div>
		</div>
	{:else if loading}
		<div class="flex h-40 items-center justify-center text-sm text-text-muted">
			<LoaderCircleIcon class="mr-2 size-4 animate-spin" /> Loading MQTT settings
		</div>
	{:else if draft && integration}
		<div class="flex min-h-[236px] flex-col lg:flex-row">
			<div
				class="flex flex-col gap-2.5 border-b border-hairline p-5 lg:w-[400px] lg:shrink-0 lg:border-r lg:border-b-0"
			>
				<div class="flex items-center gap-2.5">
					<span
						class:!bg-healthy={integration.status.state === 'connected'}
						class:!bg-activity={integration.status.state === 'connecting'}
						class:!bg-destructive={integration.status.state === 'degraded' ||
							integration.status.state === 'outbox_full'}
						class="size-2 shrink-0 rounded-full bg-text-faint"
					></span>
					<h3 id="mqtt-integration-heading" class="text-lg leading-[22px] font-semibold">MQTT 5</h3>
				</div>
				<p class="font-mono text-2xs tracking-caps text-text-faint uppercase">
					{integration.status.state.replace('_', ' ')} · MQTT 5 · {integration.status.pending_items} QUEUED
				</p>
				<p class="text-[13px] leading-[21px] text-text-muted">
					Committed event revisions and camera health transitions, delivered durably. Retained
					status lets late subscribers see current forwarder health.
				</p>
				{#if integration.status.state !== 'connected' && integration.status.state !== 'disabled'}
					<p class="text-xs leading-5 text-destructive">{integration.status.detail}</p>
				{/if}
				<div class="pt-1">
					<Button type="button" variant="outline" size="sm" onclick={beginEditing}>
						Edit broker
					</Button>
				</div>
			</div>

			<div class="flex min-w-0 flex-1 flex-col gap-4 p-5">
				<div class="flex flex-col gap-4 md:flex-row">
					<div class="min-w-0 flex-1 space-y-1.5 md:basis-[340px]">
						<p class="font-mono text-2xs tracking-caps text-text-faint">BROKER</p>
						<div
							class="flex h-[34px] items-center overflow-hidden rounded-sm border border-hairline-strong bg-raised px-2.5 font-mono text-[13px]"
						>
							<span class="truncate">{integration.configuration.broker_url}</span>
						</div>
					</div>
					<div class="min-w-0 flex-1 space-y-1.5 md:basis-[260px]">
						<p class="font-mono text-2xs tracking-caps text-text-faint">TOPIC PREFIX</p>
						<div
							class="flex h-[34px] items-center rounded-sm border border-hairline-strong bg-raised px-2.5 font-mono text-[13px]"
						>
							{integration.configuration.topic_prefix}
						</div>
					</div>
					<div class="min-w-0 flex-1 space-y-1.5 md:basis-[253px]">
						<p class="font-mono text-2xs tracking-caps text-text-faint">MQTT 5 QOS</p>
						<div class="flex h-[34px] overflow-hidden rounded-sm border border-hairline-strong">
							{#each [0, 1, 2] as qos}
								<span
									class:bg-primary={integration.configuration.qos === qos}
									class:text-on-primary={integration.configuration.qos === qos}
									class="grid min-w-0 flex-1 place-items-center text-[13px] text-text-muted"
									>{qos}</span
								>
							{/each}
						</div>
					</div>
				</div>

				<div class="space-y-1.5">
					<p class="font-mono text-2xs tracking-caps text-text-faint">TOPICS PUBLISHED</p>
					<div class="rounded-sm bg-raised px-3.5 py-3 font-mono text-xs leading-5 text-text-muted">
						<p class="break-all">
							{integration.configuration.topic_prefix}/{integration.configuration
								.instance_id}/sources/&lt;source&gt;/events/&lt;type&gt;
						</p>
						<p class="pl-4 text-text-faint">JSON revision · correlation data = stable event ID</p>
						<p class="break-all">
							{integration.configuration.topic_prefix}/{integration.configuration
								.instance_id}/forwarders/{integration.configuration.forwarder_id}/status
						</p>
						<p class="pl-4 text-text-faint">
							retained health · QoS {integration.configuration.qos} · MQTT 5 only
						</p>
					</div>
				</div>
			</div>
		</div>

		{#if error && !editing}<p
				role="alert"
				class="border-t border-hairline px-5 py-3 text-sm text-destructive"
			>
				{error}
			</p>{/if}
		{#if result && !editing}
			<p
				role="status"
				class="flex items-center gap-2 border-t border-hairline px-5 py-3 text-sm text-healthy"
			>
				<CheckIcon class="size-4" />
				{result}
			</p>
		{/if}

		{#if editing}
			<form
				class="space-y-5 border-t border-hairline bg-raised/20 p-5"
				onsubmit={save}
				data-mqtt-editor
			>
				<div class="grid gap-4 lg:grid-cols-3">
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-broker-url">
						Broker URL
						<Input
							id="mqtt-broker-url"
							bind:value={draft.brokerUrl}
							placeholder="mqtt://broker:1883"
						/>
					</label>
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-topic-prefix">
						Topic prefix
						<Input id="mqtt-topic-prefix" bind:value={draft.topicPrefix} />
					</label>
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-qos">
						QoS
						<select
							id="mqtt-qos"
							bind:value={draft.qos}
							class="h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
						>
							<option value="0">0 · At most once</option>
							<option value="1">1 · At least once</option>
							<option value="2">2 · Exactly once transport</option>
						</select>
					</label>
				</div>

				<div class="grid gap-4 lg:grid-cols-4 sm:grid-cols-2">
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-client-id">
						Client ID
						<Input id="mqtt-client-id" bind:value={draft.clientId} />
					</label>
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-instance-id">
						Instance ID
						<Input id="mqtt-instance-id" bind:value={draft.instanceId} />
					</label>
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-forwarder-id">
						Forwarder ID
						<Input id="mqtt-forwarder-id" bind:value={draft.forwarderId} />
					</label>
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-outbox-limit">
						Outbox limit (MiB)
						<Input
							id="mqtt-outbox-limit"
							type="number"
							min="1"
							max="65536"
							bind:value={draft.outboxMaxMb}
						/>
					</label>
				</div>

				<div class="grid gap-4 lg:grid-cols-3">
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-username">
						Username
						<Input id="mqtt-username" autocomplete="username" bind:value={draft.username} />
					</label>
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-password">
						Password
						<Input
							id="mqtt-password"
							type="password"
							autocomplete="new-password"
							placeholder={integration.configuration.password_configured
								? 'Configured · leave blank to keep'
								: 'Optional'}
							bind:value={draft.password}
						/>
					</label>
					<label class="space-y-1.5 text-xs font-medium" for="mqtt-tls-ca">
						TLS CA path
						<Input id="mqtt-tls-ca" bind:value={draft.tlsCaPath} placeholder="System trust" />
					</label>
				</div>

				<div class="grid gap-3 lg:grid-cols-5 sm:grid-cols-2">
					<label class="flex h-9 items-center gap-2 text-xs font-medium">
						<input type="checkbox" bind:checked={draft.enabled} /> Enabled
					</label>
					<label class="flex h-9 items-center gap-2 text-xs font-medium">
						<input type="checkbox" bind:checked={draft.retainEvents} /> Retain events
					</label>
					<label class="flex h-9 items-center gap-2 text-xs font-medium">
						<input type="checkbox" bind:checked={draft.retainHealth} /> Retain health
					</label>
					<label class="space-y-1 text-xs font-medium" for="mqtt-retry-min">
						Retry min (ms)
						<Input id="mqtt-retry-min" type="number" min="1" bind:value={draft.retryMinMs} />
					</label>
					<label class="space-y-1 text-xs font-medium" for="mqtt-retry-max">
						Retry max (ms)
						<Input id="mqtt-retry-max" type="number" min="1" bind:value={draft.retryMaxMs} />
					</label>
				</div>

				{#if integration.configuration.password_configured}
					<label class="flex items-center gap-2 text-xs text-text-muted">
						<input
							type="checkbox"
							bind:checked={draft.clearPassword}
							disabled={Boolean(draft.password)}
						/>
						Clear configured password
					</label>
				{/if}

				<div
					class="grid gap-3 rounded-sm border border-hairline bg-raised/50 p-3 lg:grid-cols-4 sm:grid-cols-2"
				>
					<div>
						<span class="text-2xs text-text-faint">OUTBOX</span>
						<p class="text-sm">
							{integration.status.pending_items} items · {bytes(integration.status.pending_bytes)}
						</p>
					</div>
					<div>
						<span class="text-2xs text-text-faint">LAST RECEIVED</span>
						<p class="text-sm">{timestamp(integration.status.last_received_at_ms)}</p>
					</div>
					<div>
						<span class="text-2xs text-text-faint">LAST DELIVERED</span>
						<p class="text-sm">{timestamp(integration.status.last_delivered_at_ms)}</p>
					</div>
					<div>
						<span class="text-2xs text-text-faint">RETRIES</span>
						<p class="text-sm">{integration.status.retry_count}</p>
					</div>
				</div>

				{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
				{#if result}
					<p role="status" class="flex items-center gap-2 text-sm text-healthy">
						<CheckIcon class="size-4" />
						{result}
					</p>
				{/if}

				<div class="flex flex-wrap gap-2">
					<Button type="submit" disabled={saving || testing}>
						{#if saving}<LoaderCircleIcon class="animate-spin" />{:else}<SaveIcon />{/if}
						Save MQTT settings
					</Button>
					<Button
						type="button"
						variant="outline"
						onclick={testConnection}
						disabled={saving || testing}
					>
						{#if testing}<LoaderCircleIcon class="animate-spin" />{:else}<PlugZapIcon />{/if}
						Test connection
					</Button>
					<Button
						type="button"
						variant="ghost"
						onclick={() => load(true)}
						disabled={saving || testing}
					>
						<RefreshCwIcon /> Refresh status
					</Button>
					<Button
						type="button"
						variant="ghost"
						onclick={cancelEditing}
						disabled={saving || testing}
					>
						Cancel
					</Button>
				</div>
			</form>
		{/if}
	{:else}
		<p role="alert" class="p-5 text-sm text-destructive">
			{error ?? 'MQTT integration is unavailable.'}
		</p>
	{/if}
</article>
