import { describe, expect, it } from 'vitest';
import { notificationsEvidence } from '$lib/notifications';

describe('notification evidence', () => {
	it('gates all notification runtime behind the exact rules capability', () => {
		expect(notificationsEvidence()).toMatchObject({
			capability: 'keeppeek.rules.v1',
			runtime: 'unavailable',
			rules: null,
			quietHours: null,
			firedLastSevenDays: null,
			deliveryHistory: null,
			retryState: null,
			browserPermission: null,
			testRuntime: null
		});
	});

	it('preserves four authored channel roles without configuration or health claims', () => {
		const channels = notificationsEvidence().channels;

		expect(channels.map((channel) => channel.id)).toEqual([
			'push',
			'email',
			'browser',
			'integrations'
		]);
		expect(
			channels.every(
				(channel) =>
					channel.configuration === null &&
					channel.health === null &&
					channel.lastDeliveryAtMs === null
			)
		).toBe(true);
	});

	it('defines target rule anatomy without synthesizing rules from events', () => {
		expect(notificationsEvidence().ruleFields).toEqual([
			'event-or-health-condition',
			'camera-or-group-scope',
			'destinations',
			'cooldown',
			'quiet-hours-policy'
		]);
		expect(notificationsEvidence().rules).toBeNull();
	});
});
