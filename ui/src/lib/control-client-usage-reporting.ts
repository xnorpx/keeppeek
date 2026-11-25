import { create } from '@bufbuild/protobuf';
import {
	UsageReportingCommandSchema,
	UsageReportingFailureKind,
	UsageReportingLifecycleState,
	type Ok,
	type Request,
	type UsageReportingPreview,
	type UsageReportingState
} from './proto/webrtc_pb';

export const usageReportingCapability = 'keeppeek.usage-reporting.v1';

export type UsageReportingConsentInput = {
	basic: boolean;
	cameraCompatibility: boolean;
	featureAdoption: boolean;
	acceptedDisclosureRevision: number;
};

export type UsageReportingPreviewInput = {
	basic: boolean;
	cameraCompatibility: boolean;
	featureAdoption: boolean;
	disclosureRevision?: number;
};

type Send = (command: Request['command'], timeoutMs?: number) => Promise<Ok['result']>;
const invalidResponseMessage = 'Server returned an invalid usage reporting response.';

export function lifecycleStateLabel(state: UsageReportingLifecycleState): string {
	switch (state) {
		case UsageReportingLifecycleState.DISABLED:
			return 'Disabled';
		case UsageReportingLifecycleState.SCHEDULED:
			return 'Scheduled';
		case UsageReportingLifecycleState.SENDING:
			return 'Sending';
		case UsageReportingLifecycleState.SUCCEEDED:
			return 'Delivered';
		case UsageReportingLifecycleState.FAILED:
			return 'Delivery failed';
		case UsageReportingLifecycleState.DELETION_PENDING:
			return 'Deletion pending';
		default:
			return 'Unknown';
	}
}

export function failureKindLabel(kind: UsageReportingFailureKind): string | null {
	switch (kind) {
		case UsageReportingFailureKind.UNSPECIFIED:
		case UsageReportingFailureKind.NONE:
			return null;
		case UsageReportingFailureKind.RECEIVER_UNCONFIGURED:
			return 'Receiver origin is not configured';
		case UsageReportingFailureKind.PAYLOAD_TOO_LARGE:
			return 'Payload exceeded the 64 KiB limit';
		case UsageReportingFailureKind.VALIDATION_FAILED:
			return 'Local schema validation failed';
		case UsageReportingFailureKind.NETWORK_UNAVAILABLE:
			return 'Network unavailable';
		case UsageReportingFailureKind.DNS_FAILED:
			return 'DNS lookup failed';
		case UsageReportingFailureKind.TLS_FAILED:
			return 'TLS verification failed';
		case UsageReportingFailureKind.TIMEOUT:
			return 'Receiver timed out';
		case UsageReportingFailureKind.RATE_LIMITED:
			return 'Receiver rate limit reached';
		case UsageReportingFailureKind.RECEIVER_REJECTED:
			return 'Receiver rejected the report';
		case UsageReportingFailureKind.RECEIVER_UNAVAILABLE:
			return 'Receiver temporarily unavailable';
		case UsageReportingFailureKind.INVALID_RESPONSE:
			return 'Receiver returned an invalid response';
		case UsageReportingFailureKind.DELETION_FAILED:
			return 'Receiver deletion request failed';
		case UsageReportingFailureKind.STORAGE_ERROR:
			return 'Local telemetry state storage failed';
		default:
			return 'Delivery error';
	}
}

export class UsageReportingControlClient {
	constructor(private readonly send: Send) {}

	async getState(): Promise<UsageReportingState> {
		const result = await this.request({ case: 'get', value: {} });
		if (result.case !== 'state') throw new Error(invalidResponseMessage);
		return validState(result.value);
	}

	async updateConsent(
		input: UsageReportingConsentInput,
		expectedConfigurationRevision: string
	): Promise<UsageReportingState> {
		if (!expectedConfigurationRevision.trim()) {
			throw new Error('Reload usage reporting settings before saving.');
		}
		if (!input.basic && (input.cameraCompatibility || input.featureAdoption)) {
			throw new Error(
				'Basic system data must be enabled before camera compatibility or feature adoption.'
			);
		}
		const result = await this.request({
			case: 'update',
			value: {
				expectedConfigurationRevision,
				basic: input.basic,
				cameraCompatibility: input.cameraCompatibility,
				featureAdoption: input.featureAdoption,
				acceptedDisclosureRevision: input.acceptedDisclosureRevision
			}
		});
		if (result.case !== 'state') throw new Error(invalidResponseMessage);
		return validState(result.value);
	}

	async preview(input: UsageReportingPreviewInput): Promise<UsageReportingPreview> {
		if (!input.basic && (input.cameraCompatibility || input.featureAdoption)) {
			throw new Error(
				'Basic system data must be enabled before previewing camera compatibility or feature adoption.'
			);
		}
		const result = await this.request({
			case: 'preview',
			value: {
				basic: input.basic,
				cameraCompatibility: input.cameraCompatibility,
				featureAdoption: input.featureAdoption,
				disclosureRevision: input.disclosureRevision ?? 0
			}
		});
		if (result.case !== 'preview') throw new Error(invalidResponseMessage);
		return result.value;
	}

	async resetIdentity(expectedConfigurationRevision: string): Promise<UsageReportingState> {
		if (!expectedConfigurationRevision.trim()) {
			throw new Error('Reload usage reporting settings before resetting installation identity.');
		}
		const result = await this.request({
			case: 'resetIdentity',
			value: { expectedConfigurationRevision }
		});
		if (result.case !== 'state') throw new Error(invalidResponseMessage);
		return validState(result.value);
	}

	async deleteData(expectedConfigurationRevision: string): Promise<UsageReportingState> {
		if (!expectedConfigurationRevision.trim()) {
			throw new Error('Reload usage reporting settings before requesting deletion.');
		}
		const result = await this.request({
			case: 'deleteData',
			value: { expectedConfigurationRevision }
		});
		if (result.case !== 'state') throw new Error(invalidResponseMessage);
		return validState(result.value);
	}

	private async request(
		action: NonNullable<Parameters<typeof create<typeof UsageReportingCommandSchema>>[1]>['action']
	) {
		const response = await this.send({
			case: 'usageReportingCommand',
			value: create(UsageReportingCommandSchema, { action })
		});
		if (response?.case !== 'usageReportingResult' || !response.value.result.case) {
			throw new Error(invalidResponseMessage);
		}
		return response.value.result;
	}
}

function validState(state: UsageReportingState): UsageReportingState {
	if (!state.consent || !state.disclosure || !state.status) {
		throw new Error(invalidResponseMessage);
	}
	return state;
}
