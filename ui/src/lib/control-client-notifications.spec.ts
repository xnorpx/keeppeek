import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import { NotificationControlClient } from './control-client-notifications';
import {
	EventAttachmentDescriptorSchema,
	NotificationClearResultSchema,
	NotificationHistoryGroupSchema,
	NotificationHistorySchema,
	NotificationInboxSchema,
	NotificationItemSchema,
	NotificationRuleResultSchema,
	type Request
} from './proto/webrtc_pb';

describe('NotificationControlClient', () => {
	it('encodes every notification clear scope without losing bigint precision', async () => {
		const commands: Request['command'][] = [];
		const client = new NotificationControlClient(async (command) => {
			commands.push(command);
			return {
				case: 'notificationRuleResult',
				value: create(NotificationRuleResultSchema, {
					result: {
						case: 'cleared',
						value: create(NotificationClearResultSchema, { clearedCount: 3n })
					}
				})
			};
		});

		await expect(client.clearScope({ kind: 'all' })).resolves.toBe(3n);
		await expect(client.clearScope({ kind: 'rule', ruleId: 'front-door' })).resolves.toBe(3n);
		await expect(
			client.clearScope({ kind: 'before', beforeMs: Number.MAX_SAFE_INTEGER })
		).resolves.toBe(3n);

		const scopes = commands.map((command) => {
			if (command.case !== 'notificationRuleCommand') {
				throw new Error('expected notification command');
			}
			const action = command.value.action;
			if (action.case !== 'clearScope') throw new Error('expected clear-scope action');
			return action.value.scope;
		});
		expect(scopes).toEqual([
			{ case: 'all', value: true },
			{ case: 'ruleId', value: 'front-door' },
			{ case: 'beforeMs', value: BigInt(Number.MAX_SAFE_INTEGER) }
		]);
	});

	it('clamps notification attachment lengths to a safe integer', async () => {
		const client = new NotificationControlClient(async () => ({
			case: 'notificationRuleResult',
			value: create(NotificationRuleResultSchema, {
				result: {
					case: 'inbox',
					value: create(NotificationInboxSchema, {
						items: [
							create(NotificationItemSchema, {
								logicalId: 'notification-1',
								stage: 'preliminary',
								severity: 'info',
								canonicalAttachment: create(EventAttachmentDescriptorSchema, {
									attachmentId: 'snapshot-1',
									byteLen: BigInt(Number.MAX_SAFE_INTEGER) + 1n
								})
							})
						]
					})
				}
			})
		}));

		const inbox = await client.getInbox();

		expect(inbox.items[0]?.canonicalAttachment?.byte_length).toBe(Number.MAX_SAFE_INTEGER);
	});

	it('fails closed on unsupported notification values and incomplete history', async () => {
		const invalidInbox = new NotificationControlClient(async () => ({
			case: 'notificationRuleResult',
			value: create(NotificationRuleResultSchema, {
				result: {
					case: 'inbox',
					value: create(NotificationInboxSchema, {
						items: [
							create(NotificationItemSchema, {
								logicalId: 'notification-1',
								stage: 'future-stage',
								severity: 'warning'
							})
						]
					})
				}
			})
		}));
		await expect(invalidInbox.getInbox()).rejects.toThrow(
			"Server returned unsupported notification stage 'future-stage'."
		);

		const incompleteHistory = new NotificationControlClient(async () => ({
			case: 'notificationRuleResult',
			value: create(NotificationRuleResultSchema, {
				result: {
					case: 'history',
					value: create(NotificationHistorySchema, {
						groups: [create(NotificationHistoryGroupSchema)]
					})
				}
			})
		}));
		await expect(incompleteHistory.getHistory()).rejects.toThrow(
			'Server returned notification history without its logical notification.'
		);
	});
});
