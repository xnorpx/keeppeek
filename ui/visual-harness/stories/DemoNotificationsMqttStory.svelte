<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import CloudNotificationSettings from '$lib/components/CloudNotificationSettings.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import MqttIntegrationPanel from '$lib/components/MqttIntegrationPanel.svelte';
	import NotificationsRuntime from '$lib/components/NotificationsRuntime.svelte';
	import type { ControlClient } from '$lib/control-client';
	import type {
		CloudNotificationSettings as CloudSettings,
		CloudNotificationStatus,
		CloudNotificationTestResult
	} from '$lib/control-client-cloud-notifications';
	import { setControlClient } from '$lib/control-context';
	import type { MqttIntegration, MqttSettingsUpdate, MqttTestResult } from '$lib/integrations';
	import {
		createNotificationRule,
		createPushoverConfig,
		type NotificationClearScope,
		type NotificationHistoryGroup,
		type NotificationInbox,
		type NotificationRuleDefinition,
		type NotificationRuleRecord
	} from '$lib/notifications';
	import BellIcon from '@lucide/svelte/icons/bell';
	import BellRingIcon from '@lucide/svelte/icons/bell-ring';
	import PlugZapIcon from '@lucide/svelte/icons/plug-zap';

	type Section = 'rules' | 'cloud' | 'mqtt';

	const baseTimeMs = Date.UTC(2026, 7, 18, 6, 37, 0);

	function createFrontDoorRule(): NotificationRuleRecord {
		const rule = createNotificationRule('front-door-person', 'Europe/Stockholm');
		rule.name = 'Front door person';
		rule.triggers = ['event_created', 'event_updated'];
		rule.filter.source_ids = ['front-door'];
		rule.schedule.quiet_hours = {
			windows: [
				{
					weekdays: ['monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday'],
					start_minute: 22 * 60,
					end_minute: 7 * 60
				}
			]
		};
		rule.actions = [
			{
				enabled: true,
				channel: 'push',
				destination: '',
				destination_configured: true,
				destination_ref: 'secret:PUSHOVER_OPS',
				pushover: {
					...createPushoverConfig(),
					device: 'ops-phone',
					sound: 'pushover',
					priority: 1
				},
				template: {
					title: 'Person at {{source.name}}',
					body: 'Verified event on {{source.id}}'
				},
				attachment: 'when_available',
				allow_second_delivery: false
			},
			{
				enabled: true,
				channel: 'browser',
				destination: '',
				template: {
					title: 'Person at {{source.name}}',
					body: 'Open {{notification.deep_link}}'
				},
				attachment: 'when_available',
				allow_second_delivery: false
			}
		];
		return {
			id: rule.id,
			ownerId: 'administrator',
			active: structuredClone(rule),
			activeRevision: 3n,
			draft: structuredClone(rule),
			draftRevision: 3n,
			createdAtMs: baseTimeMs - 86_400_000,
			updatedAtMs: baseTimeMs - 3_600_000,
			lastMatchAtMs: baseTimeMs - 45_000,
			lastDeliveryAtMs: baseTimeMs - 42_000
		};
	}

	function createDrivewayRule(): NotificationRuleRecord {
		const rule = createNotificationRule('driveway-vehicle', 'Europe/Stockholm');
		rule.name = 'Driveway vehicle';
		rule.triggers = ['event_created'];
		rule.filter.source_ids = ['driveway'];
		rule.filter.event_kinds = ['vehicle'];
		rule.cooldowns = [{ scope: 'camera_event_kind', duration_ms: 60_000 }];
		rule.actions = [
			{
				enabled: true,
				channel: 'browser',
				destination: '',
				template: {
					title: 'Vehicle at {{source.name}}',
					body: 'Open {{notification.deep_link}}'
				},
				attachment: 'when_available',
				allow_second_delivery: false
			},
			{
				enabled: true,
				channel: 'webhook',
				destination: '',
				destination_configured: true,
				destination_ref: 'secret:WEBHOOK_AUTOMATION',
				template: {
					title: 'Vehicle at {{source.name}}',
					body: 'Event recorded at {{source.id}}'
				},
				attachment: 'never',
				allow_second_delivery: false
			}
		];
		return {
			id: rule.id,
			ownerId: 'administrator',
			active: structuredClone(rule),
			activeRevision: 2n,
			draft: structuredClone(rule),
			draftRevision: 2n,
			createdAtMs: baseTimeMs - 172_800_000,
			updatedAtMs: baseTimeMs - 7_200_000,
			lastMatchAtMs: baseTimeMs - 320_000,
			lastDeliveryAtMs: baseTimeMs - 318_000
		};
	}

	function createRuleRecords(): NotificationRuleRecord[] {
		return [createFrontDoorRule(), createDrivewayRule()];
	}

	function createInboxState(): NotificationInbox {
		return {
			unreadCount: 2n,
			items: [
				{
					logicalId: 'notif-front-door-01',
					ruleId: 'front-door-person',
					sourceId: 'front-door',
					sourceIdentity: 'evt-9041',
					lifecycle: 'event',
					stage: 'enriched',
					revision: 2n,
					title: 'Person at Front Door',
					body: 'High-confidence person detection (0.94) delivered via Pushover and Browser.',
					deepLink: '/events?camera=front-door&event=evt-9041',
					attachmentAvailable: true,
					canonicalAttachment: {
						id: 'snap-9041',
						type: 'snapshot',
						content_type: 'image/jpeg',
						byte_length: 84_120,
						ordinal: 0,
						timestamp_ms: baseTimeMs - 45_000
					},
					iconKey: 'person',
					imageAvailability: 'available',
					severity: 'warning',
					createdAtMs: baseTimeMs - 45_000,
					updatedAtMs: baseTimeMs - 42_000,
					seenAtMs: null,
					acknowledgedAtMs: null
				},
				{
					logicalId: 'notif-driveway-02',
					ruleId: 'driveway-vehicle',
					sourceId: 'driveway',
					sourceIdentity: 'evt-9038',
					lifecycle: 'event',
					stage: 'preliminary',
					revision: 1n,
					title: 'Vehicle at Driveway',
					body: 'Vehicle entered the driveway zone; webhook and browser delivery completed.',
					deepLink: '/events?camera=driveway&event=evt-9038',
					attachmentAvailable: false,
					canonicalAttachment: null,
					iconKey: 'vehicle',
					imageAvailability: 'none',
					severity: 'info',
					createdAtMs: baseTimeMs - 320_000,
					updatedAtMs: baseTimeMs - 318_000,
					seenAtMs: null,
					acknowledgedAtMs: null
				}
			]
		};
	}

	function createFrontDoorHistory(
		notification: NotificationInbox['items'][number]
	): NotificationHistoryGroup {
		return {
			notification,
			events: [
				{
					sequence: 1n,
					revision: 1n,
					stage: 'preliminary',
					outcome: 'created',
					reason: 'matched front-door-person',
					occurredAtMs: baseTimeMs - 45_000,
					nextEligibleAtMs: null
				},
				{
					sequence: 2n,
					revision: 2n,
					stage: 'enriched',
					outcome: 'collapsed',
					reason: 'canonical snapshot merged under stable collapse key',
					occurredAtMs: baseTimeMs - 42_000,
					nextEligibleAtMs: null
				},
				{
					sequence: 3n,
					revision: 2n,
					stage: 'enriched',
					outcome: 'suppressed',
					reason: 'camera_event_kind cooldown active (30s)',
					occurredAtMs: baseTimeMs - 28_000,
					nextEligibleAtMs: baseTimeMs - 15_000
				}
			],
			attempts: [
				{
					sequence: 1n,
					channel: 'push',
					stage: 'enriched',
					attempt: 1,
					outcome: 'delivered',
					targetHash: '9f4a21c8e7b0',
					providerStatus: 200,
					providerRequestId: '647d2300-702c-4b38-8b2f',
					providerAcknowledgedAtMs: baseTimeMs - 40_000,
					providerExpiredAtMs: null,
					providerAcknowledgedByHash: 'ops-phone',
					providerAcknowledgementState: 'acknowledged',
					reason: 'Pushover accepted (JPEG 82 KiB)',
					attemptedAtMs: baseTimeMs - 42_000,
					retryAtMs: null
				},
				{
					sequence: 2n,
					channel: 'browser',
					stage: 'enriched',
					attempt: 1,
					outcome: 'delivered',
					targetHash: 'principal-admin',
					providerStatus: null,
					providerRequestId: null,
					providerAcknowledgedAtMs: null,
					providerExpiredAtMs: null,
					providerAcknowledgedByHash: null,
					providerAcknowledgementState: null,
					reason: 'delivered to administrator inbox',
					attemptedAtMs: baseTimeMs - 42_000,
					retryAtMs: null
				}
			]
		};
	}

	function createDrivewayHistory(
		notification: NotificationInbox['items'][number]
	): NotificationHistoryGroup {
		return {
			notification,
			events: [
				{
					sequence: 4n,
					revision: 1n,
					stage: 'preliminary',
					outcome: 'created',
					reason: 'matched driveway-vehicle',
					occurredAtMs: baseTimeMs - 320_000,
					nextEligibleAtMs: null
				},
				{
					sequence: 5n,
					revision: 1n,
					stage: 'preliminary',
					outcome: 'suppressed',
					reason: 'camera_event_kind cooldown active (60s)',
					occurredAtMs: baseTimeMs - 290_000,
					nextEligibleAtMs: baseTimeMs - 260_000
				}
			],
			attempts: [
				{
					sequence: 3n,
					channel: 'webhook',
					stage: 'preliminary',
					attempt: 1,
					outcome: 'delivered',
					targetHash: '3a7b91e044d2',
					providerStatus: 202,
					providerRequestId: null,
					providerAcknowledgedAtMs: null,
					providerExpiredAtMs: null,
					providerAcknowledgedByHash: null,
					providerAcknowledgementState: null,
					reason: 'HTTP 202 Accepted (42 ms)',
					attemptedAtMs: baseTimeMs - 318_000,
					retryAtMs: null
				}
			]
		};
	}

	function createHistoryState(inbox: NotificationInbox): NotificationHistoryGroup[] {
		const [frontDoorItem, drivewayItem] = inbox.items;
		return [createFrontDoorHistory(frontDoorItem!), createDrivewayHistory(drivewayItem!)];
	}

	function createMqttState(): MqttIntegration {
		return {
			configuration_revision: 'rev-7',
			configuration: {
				enabled: true,
				broker_url: 'mqtts://broker.home.arpa:8883',
				client_id: 'keeppeek-nvr-01',
				instance_id: 'home-nvr',
				forwarder_id: 'primary',
				topic_prefix: 'keeppeek',
				username: 'keeppeek',
				password_configured: true,
				tls_ca_path: '/etc/keeppeek/certs/ca.pem',
				qos: 1,
				retain_events: false,
				retain_health: true,
				outbox_max_mb: 64,
				retry_min_ms: 250,
				retry_max_ms: 30_000
			},
			status: {
				enabled: true,
				state: 'connected',
				detail: 'Connected to MQTT 5 broker over TLS',
				connected_at_ms: baseTimeMs - 8_700_000,
				last_received_at_ms: baseTimeMs - 8_000,
				last_delivered_at_ms: baseTimeMs - 8_000,
				pending_items: 0,
				pending_bytes: 0,
				oldest_unacknowledged_timestamp_ms: null,
				retry_count: 0,
				duplicate_count: 0,
				outbox_limit_bytes: 67_108_864
			}
		};
	}

	function createCloudHandlers(): Partial<ControlClient> {
		let cloudSettings: CloudSettings = {
			revision: 1n,
			enabled: false,
			imagePreviewsEnabled: false
		};
		let cloudStatus: CloudNotificationStatus = {
			state: 'disabled',
			paired: true,
			pending: false,
			acceptedLastHour: 14,
			acceptedLastDay: 68,
			suppressedLastHour: 3,
			suppressedLastDay: 11,
			nextEligibleAtMs: null,
			lastAcceptedAtMs: baseTimeMs - 420_000,
			lastFailure: 'none'
		};
		return {
			async getCloudNotificationSettings() {
				return cloudSettings;
			},
			async getCloudNotificationStatus() {
				return cloudStatus;
			},
			async updateCloudNotificationSettings(
				_expectedRevision: bigint,
				enabled: boolean,
				imagePreviewsEnabled: boolean
			) {
				cloudSettings = {
					revision: cloudSettings.revision + 1n,
					enabled,
					imagePreviewsEnabled
				};
				cloudStatus = { ...cloudStatus, state: enabled ? 'ready' : 'disabled' };
				return cloudSettings;
			},
			async testCloudNotification(): Promise<CloudNotificationTestResult> {
				const acceptedAt = baseTimeMs + 13_600;
				cloudStatus = {
					...cloudStatus,
					state: 'ready',
					acceptedLastHour: cloudStatus.acceptedLastHour + 1,
					acceptedLastDay: cloudStatus.acceptedLastDay + 1,
					lastAcceptedAtMs: acceptedAt,
					nextEligibleAtMs: acceptedAt + 1_200_000,
					lastFailure: 'none'
				};
				return {
					accepted: true,
					nextEligibleAtMs: acceptedAt + 1_200_000,
					failure: 'none'
				};
			}
		};
	}

	function createMqttHandlers(): Partial<ControlClient> {
		let mqtt = createMqttState();
		return {
			async getMqttIntegration() {
				return mqtt;
			},
			async updateMqttIntegration(update: MqttSettingsUpdate) {
				mqtt = {
					...mqtt,
					configuration_revision: 'rev-8',
					configuration: {
						...mqtt.configuration,
						...update,
						password_configured: update.clear_password
							? false
							: mqtt.configuration.password_configured || Boolean(update.password)
					}
				};
				return mqtt;
			},
			async testMqttIntegration(): Promise<MqttTestResult> {
				return {
					ok: true,
					kind: null,
					detail: 'Connected to mqtts://broker.home.arpa:8883 over MQTT 5 (temporary probe client).'
				};
			}
		};
	}

	function createNotificationHandlers(): Partial<ControlClient> {
		let rules = createRuleRecords();
		let inbox = createInboxState();
		const history = createHistoryState(inbox);
		return {
			async listNotificationRules() {
				return rules;
			},
			async getNotificationInbox() {
				return inbox;
			},
			async getNotificationHistory() {
				return history;
			},
			async markNotificationSeen(logicalId: string) {
				inbox = {
					items: inbox.items.map((item) =>
						item.logicalId === logicalId ? { ...item, seenAtMs: baseTimeMs } : item
					),
					unreadCount: BigInt(
						inbox.items.filter((item) => item.logicalId !== logicalId && item.seenAtMs === null)
							.length
					)
				};
			},
			async acknowledgeNotification(logicalId: string) {
				inbox = {
					...inbox,
					items: inbox.items.map((item) =>
						item.logicalId === logicalId
							? { ...item, seenAtMs: item.seenAtMs ?? baseTimeMs, acknowledgedAtMs: baseTimeMs }
							: item
					)
				};
			},
			async clearNotification(logicalId: string) {
				const remaining = inbox.items.filter((item) => item.logicalId !== logicalId);
				inbox = {
					items: remaining,
					unreadCount: BigInt(remaining.filter((item) => item.seenAtMs === null).length)
				};
			},
			async clearNotifications(_scope: NotificationClearScope) {
				const count = BigInt(inbox.items.length);
				inbox = { items: [], unreadCount: 0n };
				return count;
			},
			async saveNotificationRuleDraft(
				rule: NotificationRuleDefinition,
				expectedDraftRevision: bigint
			) {
				const existing = rules.find((record) => record.id === rule.id);
				const updated: NotificationRuleRecord = {
					id: rule.id,
					ownerId: 'administrator',
					active: existing?.active ?? null,
					activeRevision: existing?.activeRevision ?? 0n,
					draft: structuredClone(rule),
					draftRevision: expectedDraftRevision + 1n,
					createdAtMs: existing?.createdAtMs ?? baseTimeMs,
					updatedAtMs: baseTimeMs,
					lastMatchAtMs: existing?.lastMatchAtMs ?? null,
					lastDeliveryAtMs: existing?.lastDeliveryAtMs ?? null
				};
				rules = existing
					? rules.map((record) => (record.id === rule.id ? updated : record))
					: [...rules, updated];
				return updated;
			},
			async activateNotificationRule(ruleId: string) {
				const record = rules.find((candidate) => candidate.id === ruleId);
				if (!record) throw new Error('Rule not found');
				const activated: NotificationRuleRecord = {
					...record,
					active: structuredClone(record.draft),
					activeRevision: record.draftRevision
				};
				rules = rules.map((candidate) => (candidate.id === ruleId ? activated : candidate));
				return activated;
			},
			async deleteNotificationRule(ruleId: string) {
				rules = rules.filter((candidate) => candidate.id !== ruleId);
			},
			async testNotificationRule() {
				return { matchedRules: 1, createdNotifications: 1, queuedAttempts: 2 };
			}
		};
	}

	// ponytail: in-memory ControlClient double covers only the three panels shown in this demo;
	// extend with additional ControlClient methods if more settings sections join this story.
	function createDemoControlClient(): ControlClient {
		return {
			...createNotificationHandlers(),
			...createCloudHandlers(),
			...createMqttHandlers()
		} as ControlClient;
	}

	setCapabilityState([
		'keeppeek.rules.v1',
		'keeppeek.cloud-notifications.v1',
		'keeppeek.mqtt-forwarder.v1'
	]);
	setControlClient(createDemoControlClient());

	let activeSection = $state<Section>('rules');
</script>

<main
	data-paper-scenario="settings.desktop.notifications"
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="settings" />

	<section class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header
			class="flex h-14 shrink-0 items-center justify-between border-b border-hairline bg-surface px-6"
		>
			<div class="flex items-baseline gap-3">
				<h1 class="text-base font-semibold">Settings</h1>
				<span class="font-mono text-2xs tracking-caps text-text-faint">
					NOTIFICATIONS &amp; INTEGRATIONS
				</span>
			</div>
			<nav class="flex items-center gap-1.5" aria-label="Notification and integration sections">
				<button
					type="button"
					data-demo-action="tab-rules"
					aria-pressed={activeSection === 'rules'}
					class="inline-flex h-8 items-center gap-1.5 rounded-sm border px-3 text-xs font-medium {activeSection ===
					'rules'
						? 'border-primary bg-primary text-on-primary'
						: 'border-hairline bg-raised text-text-muted'}"
					onclick={() => (activeSection = 'rules')}
				>
					<BellIcon class="size-3.5" /> Rules &amp; delivery
				</button>
				<button
					type="button"
					data-demo-action="tab-cloud"
					aria-pressed={activeSection === 'cloud'}
					class="inline-flex h-8 items-center gap-1.5 rounded-sm border px-3 text-xs font-medium {activeSection ===
					'cloud'
						? 'border-primary bg-primary text-on-primary'
						: 'border-hairline bg-raised text-text-muted'}"
					onclick={() => (activeSection = 'cloud')}
				>
					<BellRingIcon class="size-3.5" /> Keeppeek Service
				</button>
				<button
					type="button"
					data-demo-action="tab-mqtt"
					aria-pressed={activeSection === 'mqtt'}
					class="inline-flex h-8 items-center gap-1.5 rounded-sm border px-3 text-xs font-medium {activeSection ===
					'mqtt'
						? 'border-primary bg-primary text-on-primary'
						: 'border-hairline bg-raised text-text-muted'}"
					onclick={() => (activeSection = 'mqtt')}
				>
					<PlugZapIcon class="size-3.5" /> MQTT 5
				</button>
			</nav>
		</header>

		<div class="min-h-0 flex-1 overflow-y-auto p-5">
			{#if activeSection === 'rules'}
				<NotificationsRuntime />
			{:else if activeSection === 'cloud'}
				<CloudNotificationSettings />
			{:else}
				<div class="overflow-hidden rounded-md border border-hairline bg-surface">
					<MqttIntegrationPanel />
				</div>
			{/if}
		</div>
	</section>
</main>
