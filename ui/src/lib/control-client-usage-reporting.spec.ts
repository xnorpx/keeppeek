import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import {
	failureKindLabel,
	lifecycleStateLabel,
	UsageReportingControlClient
} from './control-client-usage-reporting';
import {
	UsageReportingFailureKind,
	UsageReportingLifecycleState,
	UsageReportingPreviewSchema,
	UsageReportingResultSchema,
	UsageReportingStateSchema,
	type Ok,
	type Request
} from './proto/webrtc_pb';

function sampleState() {
	return create(UsageReportingStateSchema, {
		configurationRevision: 'rev-1',
		consent: {
			basic: false,
			cameraCompatibility: false,
			featureAdoption: false,
			consentedDisclosureRevision: 0
		},
		disclosure: {
			currentDisclosureRevision: 1,
			schemaVersion: '1',
			receiverOrigin: 'https://usage.example',
			schemaUrl:
				'https://github.com/xnorpx/keeppeek/blob/main/schemas/usage-reporting/v1.schema.json',
			documentationUrl: 'https://github.com/xnorpx/keeppeek/blob/main/docs/usage-reporting.md',
			aggregateDataUrl:
				'https://github.com/xnorpx/keeppeek/blob/main/examples/usage_reporting_receiver/public/aggregate-example.json',
			receiverSourceUrl:
				'https://github.com/xnorpx/keeppeek/tree/main/examples/usage_reporting_receiver',
			maxRetentionDays: 60,
			aggregateMinInstallations: 10,
			maxPayloadBytes: 65_536,
			renewalRequired: false,
			pendingRenewalFields: []
		},
		status: {
			state: UsageReportingLifecycleState.DISABLED,
			lastFailure: UsageReportingFailureKind.NONE,
			pendingDeletionCount: 0,
			identityConfigured: false,
			lastDeletionFailure: UsageReportingFailureKind.NONE
		}
	});
}

describe('UsageReportingControlClient', () => {
	it('sends typed get, update, preview, resetIdentity, and deleteData commands', async () => {
		const seen: string[] = [];
		const client = new UsageReportingControlClient(async (command: Request['command']) => {
			if (command.case !== 'usageReportingCommand') {
				throw new Error('unexpected command');
			}
			const actionCase = command.value.action.case ?? 'none';
			seen.push(actionCase);
			if (actionCase === 'preview') {
				return {
					case: 'usageReportingResult',
					value: create(UsageReportingResultSchema, {
						result: {
							case: 'preview',
							value: create(UsageReportingPreviewSchema, {
								payloadJson: '{"schema_version":1}',
								payloadBytes: 20,
								maxPayloadBytes: 65_536,
								usesPlaceholderIdentity: true,
								valid: true,
								validationError: UsageReportingFailureKind.NONE
							})
						}
					})
				} satisfies Ok['result'];
			}
			return {
				case: 'usageReportingResult',
				value: create(UsageReportingResultSchema, {
					result: { case: 'state', value: sampleState() }
				})
			} satisfies Ok['result'];
		});

		const loaded = await client.getState();
		expect(loaded.configurationRevision).toBe('rev-1');

		const preview = await client.preview({
			basic: true,
			cameraCompatibility: false,
			featureAdoption: false
		});
		expect(preview.valid).toBe(true);
		expect(preview.payloadBytes).toBe(20);

		await client.updateConsent(
			{
				basic: true,
				cameraCompatibility: true,
				featureAdoption: false,
				acceptedDisclosureRevision: 1
			},
			'rev-1'
		);
		await client.resetIdentity('rev-1');
		await client.deleteData('rev-1');

		expect(seen).toEqual(['get', 'preview', 'update', 'resetIdentity', 'deleteData']);
	});

	it('rejects category dependency violations and missing revisions before sending', async () => {
		let calls = 0;
		const client = new UsageReportingControlClient(async () => {
			calls += 1;
			return { case: undefined, value: undefined };
		});

		await expect(
			client.updateConsent(
				{
					basic: false,
					cameraCompatibility: true,
					featureAdoption: false,
					acceptedDisclosureRevision: 1
				},
				'rev-1'
			)
		).rejects.toThrow('Basic system data must be enabled');

		await expect(
			client.updateConsent(
				{
					basic: true,
					cameraCompatibility: false,
					featureAdoption: false,
					acceptedDisclosureRevision: 1
				},
				''
			)
		).rejects.toThrow('Reload usage reporting settings');

		await expect(client.resetIdentity('')).rejects.toThrow('Reload usage reporting settings');
		await expect(client.deleteData('')).rejects.toThrow('Reload usage reporting settings');
		expect(calls).toBe(0);
	});

	it('formats bounded lifecycle and failure labels without raw errors', () => {
		expect(lifecycleStateLabel(UsageReportingLifecycleState.DISABLED)).toBe('Disabled');
		expect(lifecycleStateLabel(UsageReportingLifecycleState.DELETION_PENDING)).toBe(
			'Deletion pending'
		);
		expect(failureKindLabel(UsageReportingFailureKind.NONE)).toBeNull();
		expect(failureKindLabel(UsageReportingFailureKind.TIMEOUT)).toBe('Receiver timed out');
		expect(failureKindLabel(UsageReportingFailureKind.PAYLOAD_TOO_LARGE)).toContain('64 KiB');
	});
});
