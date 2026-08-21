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
