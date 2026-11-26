import { create } from '@bufbuild/protobuf';
import { timestampDate } from '@bufbuild/protobuf/wkt';
import {
	CloudNotificationCommandSchema,
	CloudNotificationFailureCode as ProtoCloudFailureCode,
	CloudNotificationState as ProtoCloudState,
	EventImageAvailability as ProtoEventImageAvailability,
	GetCloudNotificationSettingsSchema,
	GetCloudNotificationStatusSchema,
	ResolveCloudNotificationSchema,
	TestCloudNotificationSchema,
	UpdateCloudNotificationSettingsSchema,
	type Ok,
	type Request
} from './proto/webrtc_pb';
import type { EventImageAvailability, RecordingEventAttachment } from './types';

export const CLOUD_NOTIFICATIONS_CAPABILITY_ID = 'keeppeek.cloud-notifications.v1';

export type CloudNotificationState =
	'disabled' | 'unpaired' | 'ready' | 'throttled' | 'backing_off' | 'unavailable';

export type CloudNotificationFailureCode =
	| 'none'
	| 'local_limit'
	| 'service_limit'
	| 'dependency'
	| 'credential_rejected'
	| 'integrity'
	| 'expired';

export type CloudNotificationSettings = {
	revision: bigint;
	enabled: boolean;
	imagePreviewsEnabled: boolean;
};

export type CloudNotificationStatus = {
	state: CloudNotificationState;
	paired: boolean;
	pending: boolean;
	acceptedLastHour: number;
	acceptedLastDay: number;
	suppressedLastHour: number;
	suppressedLastDay: number;
	nextEligibleAtMs: number | null;
	lastAcceptedAtMs: number | null;
	lastFailure: CloudNotificationFailureCode;
};

export type CloudNotificationTestResult = {
	accepted: boolean;
	nextEligibleAtMs: number | null;
	failure: CloudNotificationFailureCode;
};

export type CloudNotificationContext = {
	cloudEventId: Uint8Array;
	eventId: string;
	revision: bigint;
	canonicalAttachment: RecordingEventAttachment | null;
	imageAvailability: EventImageAvailability;
};

type SendRequest = (command: Request['command']) => Promise<Ok['result']>;

export class CloudNotificationControlClient {
	constructor(private readonly sendRequest: SendRequest) {}

	async getSettings(): Promise<CloudNotificationSettings> {
		const command = create(CloudNotificationCommandSchema, {
			action: { case: 'getSettings', value: create(GetCloudNotificationSettingsSchema) }
		});
		const result = await this.request(command);
		if (result.case !== 'settings') {
			throw new Error('Server returned an unexpected cloud notification settings response.');
		}
		return {
			revision: result.value.revision,
			enabled: result.value.enabled,
			imagePreviewsEnabled: result.value.imagePreviewsEnabled
		};
	}

	async updateSettings(
		expectedRevision: bigint,
		enabled: boolean,
		imagePreviewsEnabled: boolean
	): Promise<CloudNotificationSettings> {
		const command = create(CloudNotificationCommandSchema, {
			action: {
				case: 'updateSettings',
				value: create(UpdateCloudNotificationSettingsSchema, {
					expectedRevision,
					enabled,
					imagePreviewsEnabled
				})
			}
		});
		const result = await this.request(command);
		if (result.case !== 'settings') {
			throw new Error('Server returned an unexpected cloud notification update response.');
		}
		return {
			revision: result.value.revision,
			enabled: result.value.enabled,
			imagePreviewsEnabled: result.value.imagePreviewsEnabled
		};
	}

	async getStatus(): Promise<CloudNotificationStatus> {
		const command = create(CloudNotificationCommandSchema, {
			action: { case: 'getStatus', value: create(GetCloudNotificationStatusSchema) }
		});
		const result = await this.request(command);
		if (result.case !== 'status') {
			throw new Error('Server returned an unexpected cloud notification status response.');
		}
		const value = result.value;
		return {
			state: parseCloudState(value.state),
			paired: value.paired,
			pending: value.pending,
			acceptedLastHour: value.acceptedLastHour,
			acceptedLastDay: value.acceptedLastDay,
			suppressedLastHour: value.suppressedLastHour,
			suppressedLastDay: value.suppressedLastDay,
			nextEligibleAtMs: value.nextEligibleAt ? timestampDate(value.nextEligibleAt).getTime() : null,
			lastAcceptedAtMs: value.lastAcceptedAt ? timestampDate(value.lastAcceptedAt).getTime() : null,
			lastFailure: parseCloudFailureCode(value.lastFailure)
		};
	}

	async sendTest(): Promise<CloudNotificationTestResult> {
		const command = create(CloudNotificationCommandSchema, {
			action: { case: 'test', value: create(TestCloudNotificationSchema) }
		});
		const result = await this.request(command);
		if (result.case !== 'test') {
			throw new Error('Server returned an unexpected cloud notification test response.');
		}
		return {
			accepted: result.value.accepted,
			nextEligibleAtMs: result.value.nextEligibleAt
				? timestampDate(result.value.nextEligibleAt).getTime()
				: null,
			failure: parseCloudFailureCode(result.value.failure)
		};
	}

	async resolve(cloudEventId: Uint8Array): Promise<CloudNotificationContext> {
		if (cloudEventId.byteLength !== 16) {
			throw new Error('Cloud event ID must be exactly 16 bytes.');
		}
		const command = create(CloudNotificationCommandSchema, {
			action: {
				case: 'resolve',
				value: create(ResolveCloudNotificationSchema, { cloudEventId })
			}
		});
		const result = await this.request(command);
		if (result.case !== 'context') {
			throw new Error('Server returned an unexpected cloud notification context response.');
		}
		const value = result.value;
		return {
			cloudEventId: value.cloudEventId,
			eventId: value.eventId,
			revision: value.revision,
			canonicalAttachment: value.canonicalAttachment
				? {
						id: value.canonicalAttachment.attachmentId,
						type: value.canonicalAttachment.attachmentType,
						content_type: value.canonicalAttachment.contentType,
						byte_length:
							value.canonicalAttachment.byteLen === undefined
								? null
								: Number(value.canonicalAttachment.byteLen),
						ordinal: value.canonicalAttachment.ordinal,
						timestamp_ms: value.canonicalAttachment.timestamp
							? timestampDate(value.canonicalAttachment.timestamp).getTime()
							: null,
						text: value.canonicalAttachment.text ?? null
					}
				: null,
			imageAvailability: parseImageAvailability(value.imageAvailability)
		};
	}

	private async request(command: ReturnType<typeof create<typeof CloudNotificationCommandSchema>>) {
		const result = await this.sendRequest({
			case: 'cloudNotificationCommand',
			value: command
		});
		if (result.case !== 'cloudNotificationResult') {
			throw new Error('Server returned an unexpected cloud notification response.');
		}
		return result.value.result;
	}
}

export function parseCloudState(state: ProtoCloudState): CloudNotificationState {
	switch (state) {
		case ProtoCloudState.DISABLED:
			return 'disabled';
		case ProtoCloudState.UNPAIRED:
			return 'unpaired';
		case ProtoCloudState.READY:
			return 'ready';
		case ProtoCloudState.THROTTLED:
			return 'throttled';
		case ProtoCloudState.BACKING_OFF:
			return 'backing_off';
		case ProtoCloudState.UNAVAILABLE:
			return 'unavailable';
		default:
			throw new Error('Server returned an unsupported cloud notification state.');
	}
}

export function parseCloudFailureCode(code: ProtoCloudFailureCode): CloudNotificationFailureCode {
	switch (code) {
		case ProtoCloudFailureCode.NONE:
			return 'none';
		case ProtoCloudFailureCode.LOCAL_LIMIT:
			return 'local_limit';
		case ProtoCloudFailureCode.SERVICE_LIMIT:
			return 'service_limit';
		case ProtoCloudFailureCode.DEPENDENCY:
			return 'dependency';
		case ProtoCloudFailureCode.CREDENTIAL_REJECTED:
			return 'credential_rejected';
		case ProtoCloudFailureCode.INTEGRITY:
			return 'integrity';
		case ProtoCloudFailureCode.EXPIRED:
			return 'expired';
		default:
			throw new Error('Server returned an unsupported cloud notification failure code.');
	}
}

function parseImageAvailability(value: ProtoEventImageAvailability): EventImageAvailability {
	switch (value) {
		case ProtoEventImageAvailability.NONE:
			return 'none';
		case ProtoEventImageAvailability.AVAILABLE:
			return 'available';
		case ProtoEventImageAvailability.UNAVAILABLE:
			return 'unavailable';
		default:
			throw new Error('Server returned an unsupported event image availability value.');
	}
}
