import { create } from '@bufbuild/protobuf';
import { timestampFromDate } from '@bufbuild/protobuf/wkt';
import { describe, expect, it, vi } from 'vitest';
import {
	CloudNotificationControlClient,
	parseCloudFailureCode,
	parseCloudState
} from './control-client-cloud-notifications';
import {
	CloudNotificationFailureCode,
	CloudNotificationResultSchema,
	CloudNotificationSettingsSchema,
	CloudNotificationState,
	CloudNotificationStatusSchema,
	CloudNotificationTestResultSchema,
	type Ok
} from './proto/webrtc_pb';

describe('CloudNotificationControlClient', () => {
	it('reads settings, updates with optimistic revision, inspects status, and sends tests', async () => {
		const nextEligible = new Date('2026-09-19T12:30:00.000Z');
		const lastAccepted = new Date('2026-09-19T12:00:00.000Z');
		const sendRequest = vi
			.fn()
			.mockResolvedValueOnce({
				case: 'cloudNotificationResult',
				value: create(CloudNotificationResultSchema, {
					result: {
						case: 'settings',
						value: create(CloudNotificationSettingsSchema, {
							revision: 3n,
							enabled: false,
							imagePreviewsEnabled: false
						})
					}
				})
			} satisfies Ok['result'])
			.mockResolvedValueOnce({
				case: 'cloudNotificationResult',
				value: create(CloudNotificationResultSchema, {
					result: {
						case: 'settings',
						value: create(CloudNotificationSettingsSchema, {
							revision: 4n,
							enabled: true,
							imagePreviewsEnabled: false
						})
					}
				})
			} satisfies Ok['result'])
			.mockResolvedValueOnce({
				case: 'cloudNotificationResult',
				value: create(CloudNotificationResultSchema, {
					result: {
						case: 'status',
						value: create(CloudNotificationStatusSchema, {
							state: CloudNotificationState.THROTTLED,
							paired: true,
							pending: true,
							acceptedLastHour: 120,
							acceptedLastDay: 240,
							suppressedLastHour: 15,
							suppressedLastDay: 40,
							nextEligibleAt: timestampFromDate(nextEligible),
							lastAcceptedAt: timestampFromDate(lastAccepted),
							lastFailure: CloudNotificationFailureCode.LOCAL_LIMIT
						})
					}
				})
			} satisfies Ok['result'])
			.mockResolvedValueOnce({
				case: 'cloudNotificationResult',
				value: create(CloudNotificationResultSchema, {
					result: {
						case: 'test',
						value: create(CloudNotificationTestResultSchema, {
							accepted: false,
							nextEligibleAt: timestampFromDate(nextEligible),
							failure: CloudNotificationFailureCode.LOCAL_LIMIT
						})
					}
				})
			} satisfies Ok['result']);

		const client = new CloudNotificationControlClient(sendRequest);
		const settings = await client.getSettings();
		expect(settings).toEqual({
			revision: 3n,
			enabled: false,
			imagePreviewsEnabled: false
		});

		const updated = await client.updateSettings(3n, true, false);
		expect(updated.revision).toBe(4n);
		expect(updated.enabled).toBe(true);

		const status = await client.getStatus();
		expect(status).toEqual({
			state: 'throttled',
			paired: true,
			pending: true,
			acceptedLastHour: 120,
			acceptedLastDay: 240,
			suppressedLastHour: 15,
			suppressedLastDay: 40,
			nextEligibleAtMs: nextEligible.getTime(),
			lastAcceptedAtMs: lastAccepted.getTime(),
			lastFailure: 'local_limit'
		});

		const test = await client.sendTest();
		expect(test).toEqual({
			accepted: false,
			nextEligibleAtMs: nextEligible.getTime(),
			failure: 'local_limit'
		});
	});

	it('fails closed on unknown enums and invalid cloud_event_id length', async () => {
		const client = new CloudNotificationControlClient(vi.fn());
		await expect(client.resolve(new Uint8Array(15))).rejects.toThrow(/16 bytes/);
		expect(() => parseCloudState(CloudNotificationState.UNSPECIFIED)).toThrow();
		expect(() => parseCloudFailureCode(CloudNotificationFailureCode.UNSPECIFIED)).toThrow();
	});
});

describe('NotificationControlClient keeppeek_service channel', () => {
	it('accepts keeppeek_service in delivery attempt history', async () => {
		const { NotificationControlClient } = await import('./control-client-notifications');
		const {
			NotificationDeliveryAttemptSchema,
			NotificationHistoryGroupSchema,
			NotificationHistorySchema,
			NotificationItemSchema,
			NotificationRuleResultSchema
		} = await import('./proto/webrtc_pb');
		const sendRequest = vi.fn().mockResolvedValueOnce({
			case: 'notificationRuleResult',
			value: create(NotificationRuleResultSchema, {
				result: {
					case: 'history',
					value: create(NotificationHistorySchema, {
						groups: [
							create(NotificationHistoryGroupSchema, {
								notification: create(NotificationItemSchema, {
									logicalId: 'logical-1',
									ruleId: 'rule-1',
									sourceId: 'cam-1',
									sourceIdentity: 'evt-1',
									lifecycle: 'event',
									stage: 'preliminary',
									revision: 1n,
									title: 'Alert',
									body: 'Body',
									deepLink: '/events/1',
									severity: 'info',
									createdAtMs: 1000n,
									updatedAtMs: 1000n
								}),
								events: [],
								attempts: [
									create(NotificationDeliveryAttemptSchema, {
										sequence: 1n,
										channel: 'keeppeek_service',
										stage: 'preliminary',
										attempt: 1,
										outcome: 'delivered',
										targetHash: 'abc',
										attemptedAtMs: 1000n
									})
								]
							})
						]
					})
				}
			})
		} satisfies Ok['result']);
		const client = new NotificationControlClient(sendRequest);
		const groups = await client.getHistory();
		expect(groups[0]?.attempts[0]?.channel).toBe('keeppeek_service');
	});
});
