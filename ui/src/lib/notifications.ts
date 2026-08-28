export type NotificationChannelTarget = {
	id: 'push' | 'email' | 'browser' | 'integrations';
	label: string;
	intendedBehavior: string;
	configuration: null;
	health: null;
	lastDeliveryAtMs: null;
};

export type NotificationRuleField =
	| 'event-or-health-condition'
	| 'camera-or-group-scope'
	| 'destinations'
	| 'cooldown'
	| 'quiet-hours-policy';

export type NotificationsEvidence = {
	capability: 'keeppeek.rules.v1';
	runtime: 'unavailable';
	channels: readonly NotificationChannelTarget[];
	rules: null;
	ruleFields: readonly NotificationRuleField[];
	quietHours: null;
	firedLastSevenDays: null;
	deliveryHistory: null;
	retryState: null;
	browserPermission: null;
	testRuntime: null;
};

const channels = Object.freeze<NotificationChannelTarget[]>([
	{
		id: 'push',
		label: 'Push to phones',
		intendedBehavior: 'Human-facing urgent delivery through a configured push provider.',
		configuration: null,
		health: null,
		lastDeliveryAtMs: null
	},
	{
		id: 'email',
		label: 'Email',
		intendedBehavior: 'Human-facing non-urgent delivery and digest transport.',
		configuration: null,
		health: null,
		lastDeliveryAtMs: null
	},
	{
		id: 'browser',
		label: 'In this browser',
		intendedBehavior: 'Local toast or browser notification while a supported client is active.',
		configuration: null,
		health: null,
		lastDeliveryAtMs: null
	},
	{
		id: 'integrations',
		label: 'MQTT and webhooks',
		intendedBehavior:
			'Machine-to-machine event delivery belongs to integration runtimes, not human rules.',
		configuration: null,
		health: null,
		lastDeliveryAtMs: null
	}
]);

export function notificationsEvidence(): NotificationsEvidence {
	return {
		capability: 'keeppeek.rules.v1',
		runtime: 'unavailable',
		channels,
		rules: null,
		ruleFields: [
			'event-or-health-condition',
			'camera-or-group-scope',
			'destinations',
			'cooldown',
			'quiet-hours-policy'
		],
		quietHours: null,
		firedLastSevenDays: null,
		deliveryHistory: null,
		retryState: null,
		browserPermission: null,
		testRuntime: null
	};
}

export type NotificationTrigger =
	| 'event_created'
	| 'event_updated'
	| 'event_ended'
	| 'outage_started'
	| 'recovery'
	| 'storage_health'
	| 'recording_health'
	| 'test';

export type NotificationSeverity = 'info' | 'warning' | 'critical';
export type NotificationStage = 'preliminary' | 'enriched' | 'recovery';
export type NotificationChannel = 'browser' | 'push' | 'webhook' | 'forwarder';
export type NotificationAttachmentPolicy = 'never' | 'when_available' | 'required';
export type PushoverPriority = -2 | -1 | 0 | 1 | 2;
export type PushoverPublicConfig = {
	device: string | null;
	sound: string | null;
	priority: PushoverPriority;
	retry_seconds: number | null;
	expire_seconds: number | null;
	deep_link_base_url: string | null;
};
export type NotificationCooldownScope = 'event' | 'camera_event_kind' | 'group' | 'rule' | 'outage';
export type NotificationRateLimitScope = 'rule' | 'channel' | 'principal' | 'global';
export type NotificationWeekday =
	'monday' | 'tuesday' | 'wednesday' | 'thursday' | 'friday' | 'saturday' | 'sunday';

export type NotificationWeeklyWindow = {
	weekdays: NotificationWeekday[];
	start_minute: number;
	end_minute: number;
};

export type NotificationRuleDefinition = {
	id: string;
	name: string;
	enabled: boolean;
	revision: number;
	owner_id: string;
	triggers: NotificationTrigger[];
	filter: {
		source_ids: string[];
		group_ids: string[];
		event_kinds: string[];
		zones: string[];
		minimum_confidence: number | null;
		attachment_required: boolean | null;
		minimum_duration_ms: number | null;
		severities: NotificationSeverity[];
		reviewed: boolean | null;
		bookmarked: boolean | null;
	};
	schedule: {
		timezone: string;
		active_windows: NotificationWeeklyWindow[];
		quiet_hours: { windows: NotificationWeeklyWindow[] } | null;
	};
	cooldowns: Array<{ scope: NotificationCooldownScope; duration_ms: number }>;
	rate_limits: Array<{
		scope: NotificationRateLimitScope;
		maximum: number;
		window_ms: number;
	}>;
	critical_bypass: { maximum: number; window_ms: number } | null;
	enrichment: {
		deadline_ms: number;
		maximum_revisions: number;
		maximum_attempts: number;
		maximum_attachment_bytes: number;
		wake_after_deadline: boolean;
	};
	actions: Array<{
		enabled: boolean;
		channel: NotificationChannel;
		destination: string;
		destination_configured?: boolean;
		destination_ref?: string;
		pushover?: PushoverPublicConfig;
		template: { title: string; body: string };
		attachment: NotificationAttachmentPolicy;
		allow_second_delivery: boolean;
	}>;
	failure: {
		maximum_attempts: number;
		maximum_retry_interval_ms: number;
		expiry_ms: number;
	};
};

export function createPushoverConfig(): PushoverPublicConfig {
	return {
		device: null,
		sound: null,
		priority: 0,
		retry_seconds: null,
		expire_seconds: null,
		deep_link_base_url: null
	};
}

export type NotificationRuleRecord = {
	id: string;
	ownerId: string;
	active: NotificationRuleDefinition | null;
	activeRevision: bigint;
	draft: NotificationRuleDefinition;
	draftRevision: bigint;
	createdAtMs: number;
	updatedAtMs: number;
	lastMatchAtMs: number | null;
	lastDeliveryAtMs: number | null;
};

export type NotificationItem = {
	logicalId: string;
	ruleId: string;
	sourceId: string;
	sourceIdentity: string;
	lifecycle: string;
	stage: NotificationStage;
	revision: bigint;
	title: string;
	body: string;
	deepLink: string;
	attachmentAvailable: boolean;
	canonicalAttachment: import('./types').RecordingEventAttachment | null;
	iconKey: import('./types').EventIconKey | undefined;
	imageAvailability: import('./types').EventImageAvailability;
	severity: NotificationSeverity;
	createdAtMs: number;
	updatedAtMs: number;
	seenAtMs: number | null;
	acknowledgedAtMs: number | null;
};

export type NotificationInbox = {
	items: NotificationItem[];
	unreadCount: bigint;
};

export type NotificationHistoryEvent = {
	sequence: bigint;
	revision: bigint;
	stage: NotificationStage;
	outcome: string;
	reason: string | null;
	occurredAtMs: number;
	nextEligibleAtMs: number | null;
};

export type NotificationDeliveryAttempt = {
	sequence: bigint;
	channel: NotificationChannel;
	stage: NotificationStage;
	attempt: number;
	outcome: string;
	targetHash: string;
	providerStatus: number | null;
	providerRequestId: string | null;
	providerAcknowledgedAtMs: number | null;
	providerExpiredAtMs: number | null;
	providerAcknowledgedByHash: string | null;
	providerAcknowledgementState: 'pending' | 'acknowledged' | 'expired' | 'failed' | null;
	reason: string | null;
	attemptedAtMs: number;
	retryAtMs: number | null;
};

export type NotificationHistoryGroup = {
	notification: NotificationItem;
	events: NotificationHistoryEvent[];
	attempts: NotificationDeliveryAttempt[];
};

export type NotificationTestResult = {
	matchedRules: number;
	createdNotifications: number;
	queuedAttempts: number;
};

export type NotificationClearScope =
	{ kind: 'all' } | { kind: 'rule'; ruleId: string } | { kind: 'before'; beforeMs: number };

export function createNotificationRule(id: string, timezone: string): NotificationRuleDefinition {
	return {
		id,
		name: 'Person alert',
		enabled: true,
		revision: 0,
		owner_id: '',
		triggers: ['event_created', 'event_updated', 'event_ended'],
		filter: {
			source_ids: [],
			group_ids: [],
			event_kinds: ['person'],
			zones: [],
			minimum_confidence: null,
			attachment_required: null,
			minimum_duration_ms: null,
			severities: [],
			reviewed: null,
			bookmarked: null
		},
		schedule: {
			timezone,
			active_windows: [],
			quiet_hours: null
		},
		cooldowns: [{ scope: 'camera_event_kind', duration_ms: 30_000 }],
		rate_limits: [{ scope: 'rule', maximum: 20, window_ms: 60_000 }],
		critical_bypass: null,
		enrichment: {
			deadline_ms: 10_000,
			maximum_revisions: 4,
			maximum_attempts: 2,
			maximum_attachment_bytes: 1_048_576,
			wake_after_deadline: false
		},
		actions: [
			{
				enabled: true,
				channel: 'browser',
				destination: '',
				template: {
					title: '{{event.kind}} at {{source.id}}',
					body: 'Open {{notification.deep_link}}'
				},
				attachment: 'when_available',
				allow_second_delivery: false
			}
		],
		failure: {
			maximum_attempts: 4,
			maximum_retry_interval_ms: 60_000,
			expiry_ms: 3_600_000
		}
	};
}

export function parseNotificationRuleDefinition(value: string): NotificationRuleDefinition {
	const parsed: unknown = JSON.parse(value);
	if (!parsed || typeof parsed !== 'object' || !('id' in parsed) || !('actions' in parsed)) {
		throw new Error('Server returned an invalid notification rule definition.');
	}
	return parsed as NotificationRuleDefinition;
}
