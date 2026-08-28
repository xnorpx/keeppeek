import { create } from '@bufbuild/protobuf';
import { timestampDate } from '@bufbuild/protobuf/wkt';
import {
	AcknowledgeNotificationSchema,
	ActivateNotificationRuleSchema,
	ClearNotificationSchema,
	ClearNotificationsSchema,
	DeleteNotificationRuleSchema,
	GetNotificationHistorySchema,
	GetNotificationInboxSchema,
	ListNotificationRulesSchema,
	MarkNotificationSeenSchema,
	EventImageAvailability as ProtoEventImageAvailability,
	NotificationRuleCommandSchema,
	SaveNotificationRuleDraftSchema,
	TestNotificationRuleSchema,
	type NotificationDeliveryAttempt as ProtoNotificationDeliveryAttempt,
	type NotificationHistoryEvent as ProtoNotificationHistoryEvent,
	type NotificationHistoryGroup as ProtoNotificationHistoryGroup,
	type NotificationInbox as ProtoNotificationInbox,
	type NotificationItem as ProtoNotificationItem,
	type NotificationRuleRecord as ProtoNotificationRuleRecord,
	type EventAttachmentDescriptor as ProtoEventAttachmentDescriptor,
	type Ok,
	type Request
} from './proto/webrtc_pb';
import { eventIconKey } from './event-presentation';
import {
	parseNotificationRuleDefinition,
	type NotificationChannel,
	type NotificationClearScope,
	type NotificationDeliveryAttempt,
	type NotificationHistoryEvent,
	type NotificationHistoryGroup,
	type NotificationInbox,
	type NotificationItem,
	type NotificationRuleDefinition,
	type NotificationRuleRecord,
	type NotificationSeverity,
	type NotificationStage,
	type NotificationTestResult
} from './notifications';
import type {
	EventImageAvailability as EventImageAvailabilityState,
	RecordingEventAttachment
} from './types';

type SendRequest = (command: Request['command']) => Promise<Ok['result']>;

export class NotificationControlClient {
	constructor(private readonly sendRequest: SendRequest) {}

	async listRules(): Promise<NotificationRuleRecord[]> {
		const command = create(NotificationRuleCommandSchema, {
			action: { case: 'listRules', value: create(ListNotificationRulesSchema) }
		});
		const result = await this.request(command);
		if (result.case !== 'rules') {
			throw new Error('Server returned an unexpected notification rule list response.');
		}
		return result.value.rules.map(notificationRuleRecord);
	}

	async saveRuleDraft(
		rule: NotificationRuleDefinition,
		expectedDraftRevision: bigint
	): Promise<NotificationRuleRecord> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'saveDraft',
				value: create(SaveNotificationRuleDraftSchema, {
					definitionJson: JSON.stringify(rule),
					expectedDraftRevision
				})
			}
		});
		return this.ruleMutation(command);
	}

	async activateRule(
		ruleId: string,
		expectedActiveRevision: bigint,
		expectedDraftRevision: bigint
	): Promise<NotificationRuleRecord> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'activate',
				value: create(ActivateNotificationRuleSchema, {
					ruleId,
					expectedActiveRevision,
					expectedDraftRevision
				})
			}
		});
		return this.ruleMutation(command);
	}

	async deleteRule(
		ruleId: string,
		expectedActiveRevision: bigint,
		expectedDraftRevision: bigint
	): Promise<void> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'delete',
				value: create(DeleteNotificationRuleSchema, {
					ruleId,
					expectedActiveRevision,
					expectedDraftRevision
				})
			}
		});
		const result = await this.request(command);
		if (result.case !== 'mutation' || result.value.logicalId !== ruleId) {
			throw new Error('Server returned an unexpected notification rule deletion response.');
		}
	}

	async testRule(ruleId: string): Promise<NotificationTestResult> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'test',
				value: create(TestNotificationRuleSchema, { ruleId })
			}
		});
		const result = await this.request(command);
		if (result.case !== 'test') {
			throw new Error('Server returned an unexpected notification test response.');
		}
		return {
			matchedRules: result.value.matchedRules,
			createdNotifications: result.value.createdNotifications,
			queuedAttempts: result.value.queuedAttempts
		};
	}

	async getInbox(limit = 100): Promise<NotificationInbox> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'getInbox',
				value: create(GetNotificationInboxSchema, { limit })
			}
		});
		const result = await this.request(command);
		if (result.case !== 'inbox') {
			throw new Error('Server returned an unexpected notification inbox response.');
		}
		return notificationInbox(result.value);
	}

	async getHistory(limit = 100): Promise<NotificationHistoryGroup[]> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'getHistory',
				value: create(GetNotificationHistorySchema, { limit })
			}
		});
		const result = await this.request(command);
		if (result.case !== 'history') {
			throw new Error('Server returned an unexpected notification history response.');
		}
		return result.value.groups.map(notificationHistoryGroup);
	}

	async markSeen(logicalId: string): Promise<void> {
		await this.receiptMutation(
			logicalId,
			create(NotificationRuleCommandSchema, {
				action: {
					case: 'markSeen',
					value: create(MarkNotificationSeenSchema, { logicalId })
				}
			})
		);
	}

	async acknowledge(logicalId: string): Promise<void> {
		await this.receiptMutation(
			logicalId,
			create(NotificationRuleCommandSchema, {
				action: {
					case: 'acknowledge',
					value: create(AcknowledgeNotificationSchema, { logicalId })
				}
			})
		);
	}

	async clear(logicalId: string): Promise<void> {
		await this.receiptMutation(
			logicalId,
			create(NotificationRuleCommandSchema, {
				action: {
					case: 'clear',
					value: create(ClearNotificationSchema, { logicalId })
				}
			})
		);
	}

	async clearScope(scope: NotificationClearScope): Promise<bigint> {
		const wireScope =
			scope.kind === 'all'
				? ({ case: 'all', value: true } as const)
				: scope.kind === 'rule'
					? ({ case: 'ruleId', value: scope.ruleId } as const)
					: ({ case: 'beforeMs', value: BigInt(scope.beforeMs) } as const);
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'clearScope',
				value: create(ClearNotificationsSchema, { scope: wireScope })
			}
		});
		const result = await this.request(command);
		if (result.case !== 'cleared') {
			throw new Error('Server returned an unexpected notification clear response.');
		}
		return result.value.clearedCount;
	}

	private async request(command: ReturnType<typeof create<typeof NotificationRuleCommandSchema>>) {
		const result = await this.sendRequest({ case: 'notificationRuleCommand', value: command });
		if (result.case !== 'notificationRuleResult' || !result.value.result.case) {
			throw new Error('Server returned an unexpected notification response.');
		}
		return result.value.result;
	}

	private async ruleMutation(
		command: ReturnType<typeof create<typeof NotificationRuleCommandSchema>>
	): Promise<NotificationRuleRecord> {
		const result = await this.request(command);
		if (result.case !== 'rule') {
			throw new Error('Server returned an unexpected notification rule response.');
		}
		return notificationRuleRecord(result.value);
	}

	private async receiptMutation(
		logicalId: string,
		command: ReturnType<typeof create<typeof NotificationRuleCommandSchema>>
	): Promise<void> {
		const result = await this.request(command);
		if (result.case !== 'mutation' || result.value.logicalId !== logicalId) {
			throw new Error('Server returned an unexpected notification receipt response.');
		}
	}
}

function notificationRuleRecord(record: ProtoNotificationRuleRecord): NotificationRuleRecord {
	return {
		id: record.ruleId,
		ownerId: record.ownerId,
		active: record.activeDefinitionJson
			? parseNotificationRuleDefinition(record.activeDefinitionJson)
			: null,
		activeRevision: record.activeRevision,
		draft: parseNotificationRuleDefinition(record.draftDefinitionJson),
		draftRevision: record.draftRevision,
		createdAtMs: Number(record.createdAtMs),
		updatedAtMs: Number(record.updatedAtMs),
		lastMatchAtMs: record.lastMatchAtMs === undefined ? null : Number(record.lastMatchAtMs),
		lastDeliveryAtMs: record.lastDeliveryAtMs === undefined ? null : Number(record.lastDeliveryAtMs)
	};
}

function notificationInbox(inbox: ProtoNotificationInbox): NotificationInbox {
	return {
		items: inbox.items.map(notificationItem),
		unreadCount: inbox.unreadCount
	};
}

function notificationItem(item: ProtoNotificationItem): NotificationItem {
	const canonicalAttachment = item.canonicalAttachment
		? recordingEventAttachment(item.canonicalAttachment)
		: null;
	return {
		logicalId: item.logicalId,
		ruleId: item.ruleId,
		sourceId: item.sourceId,
		sourceIdentity: item.sourceIdentity,
		lifecycle: item.lifecycle,
		stage: notificationStage(item.stage),
		revision: item.revision,
		title: item.title,
		body: item.body,
		deepLink: item.deepLink,
		attachmentAvailable: item.attachmentAvailable,
		canonicalAttachment,
		iconKey: item.iconKey ? eventIconKey(item.iconKey, '') : undefined,
		imageAvailability: eventImageAvailability(item.imageAvailability, canonicalAttachment !== null),
		severity: notificationSeverity(item.severity),
		createdAtMs: Number(item.createdAtMs),
		updatedAtMs: Number(item.updatedAtMs),
		seenAtMs: item.seenAtMs === undefined ? null : Number(item.seenAtMs),
		acknowledgedAtMs: item.acknowledgedAtMs === undefined ? null : Number(item.acknowledgedAtMs)
	};
}

function recordingEventAttachment(
	attachment: ProtoEventAttachmentDescriptor
): RecordingEventAttachment {
	return {
		id: attachment.attachmentId,
		type: attachment.attachmentType,
		content_type: attachment.contentType,
		byte_length: attachment.byteLen === undefined ? null : Number(attachment.byteLen),
		ordinal: attachment.ordinal,
		timestamp_ms: attachment.timestamp ? timestampDate(attachment.timestamp).getTime() : null,
		text: attachment.text ?? null
	};
}

function eventImageAvailability(
	availability: ProtoEventImageAvailability,
	hasCanonicalImage: boolean
): EventImageAvailabilityState {
	if (availability === ProtoEventImageAvailability.AVAILABLE) return 'available';
	if (availability === ProtoEventImageAvailability.UNAVAILABLE) return 'unavailable';
	if (availability === ProtoEventImageAvailability.NONE) return 'none';
	return hasCanonicalImage ? 'available' : 'none';
}

function notificationHistoryGroup(group: ProtoNotificationHistoryGroup): NotificationHistoryGroup {
	if (!group.notification) {
		throw new Error('Server returned notification history without its logical notification.');
	}
	return {
		notification: notificationItem(group.notification),
		events: group.events.map(notificationHistoryEvent),
		attempts: group.attempts.map(notificationDeliveryAttempt)
	};
}

function notificationHistoryEvent(event: ProtoNotificationHistoryEvent): NotificationHistoryEvent {
	return {
		sequence: event.sequence,
		revision: event.revision,
		stage: notificationStage(event.stage),
		outcome: event.outcome,
		reason: event.reason ?? null,
		occurredAtMs: Number(event.occurredAtMs),
		nextEligibleAtMs: event.nextEligibleAtMs === undefined ? null : Number(event.nextEligibleAtMs)
	};
}

function notificationDeliveryAttempt(
	attempt: ProtoNotificationDeliveryAttempt
): NotificationDeliveryAttempt {
	return {
		sequence: attempt.sequence,
		channel: notificationChannel(attempt.channel),
		stage: notificationStage(attempt.stage),
		attempt: attempt.attempt,
		outcome: attempt.outcome,
		targetHash: attempt.targetHash,
		providerStatus: attempt.providerStatus ?? null,
		providerRequestId: attempt.providerRequestId ?? null,
		providerAcknowledgedAtMs:
			attempt.providerAcknowledgedAtMs === undefined
				? null
				: Number(attempt.providerAcknowledgedAtMs),
		providerExpiredAtMs:
			attempt.providerExpiredAtMs === undefined ? null : Number(attempt.providerExpiredAtMs),
		providerAcknowledgedByHash: attempt.providerAcknowledgedByHash ?? null,
		providerAcknowledgementState: providerAcknowledgementState(
			attempt.providerAcknowledgementState
		),
		reason: attempt.reason ?? null,
		attemptedAtMs: Number(attempt.attemptedAtMs),
		retryAtMs: attempt.retryAtMs === undefined ? null : Number(attempt.retryAtMs)
	};
}

function providerAcknowledgementState(
	value: string | undefined
): 'pending' | 'acknowledged' | 'expired' | 'failed' | null {
	if (value === undefined || value === '') return null;
	if (
		value === 'pending' ||
		value === 'acknowledged' ||
		value === 'expired' ||
		value === 'failed'
	) {
		return value;
	}
	throw new Error(`Server returned unsupported provider acknowledgement state '${value}'.`);
}

function notificationStage(value: string): NotificationStage {
	if (value === 'preliminary' || value === 'enriched' || value === 'recovery') return value;
	throw new Error(`Server returned unsupported notification stage '${value}'.`);
}

function notificationSeverity(value: string): NotificationSeverity {
	if (value === 'info' || value === 'warning' || value === 'critical') return value;
	throw new Error(`Server returned unsupported notification severity '${value}'.`);
}

function notificationChannel(value: string): NotificationChannel {
	if (value === 'browser' || value === 'push' || value === 'webhook' || value === 'forwarder') {
		return value;
	}
	throw new Error(`Server returned unsupported notification channel '${value}'.`);
}
