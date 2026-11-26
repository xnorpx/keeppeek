import { create } from '@bufbuild/protobuf';
import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import { usageReportingCapability } from '../src/lib/control-client-usage-reporting';
import {
	UsageReportingFailureKind,
	UsageReportingLifecycleState,
	UsageReportingPreviewSchema,
	UsageReportingResultSchema,
	UsageReportingStateSchema,
	type UsageReportingCommand
} from '../src/lib/proto/webrtc_pb';

const receiverOrigin = 'https://usage-receiver.invalid';

function isReceiverRequest(url: string): boolean {
	try {
		return new URL(url).origin === receiverOrigin;
	} catch {
		return false;
	}
}

const runtimeConfiguration = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: 1,
	storage: {
		medium_term_path: '/recordings',
		long_term_path: '/recordings',
		recording_catalog_path: '/recordings/recordings.db',
		event_thumbnail_path: '/recordings/.event-thumbnails',
		event_thumbnail_max_mb: 1024,
		short_term_secs: 120,
		medium_term_secs: 1800,
		flush_interval_secs: 60,
		write_buffer_bytes: 8192,
		long_term_max_gb: 2048
	},
	recording_estimate: {
		estimated_bitrate_bps: 0,
		bytes_per_day: 0,
		known_streams: 0,
		unknown_streams: 0,
		estimated_retention_days: null
	}
};

function createUsageHarness(options?: {
	initialBasic?: boolean;
	consentedRevision?: number;
	currentRevision?: number;
	offlineDeletes?: boolean;
}) {
	let revision = 1;
	let basic = options?.initialBasic ?? false;
	let cameraCompatibility = false;
	let featureAdoption = false;
	let consentedDisclosureRevision = options?.consentedRevision ?? (basic ? 1 : 0);
	const currentDisclosureRevision = options?.currentRevision ?? 1;
	let identityConfigured = basic;
	let pendingDeletionCount = 0;
	let offlineDeletes = options?.offlineDeletes ?? false;
	let lastDeletionFailure = UsageReportingFailureKind.NONE;
	const actions: string[] = [];

	const currentState = () =>
		create(UsageReportingStateSchema, {
			configurationRevision: `rev-${revision}`,
			consent: {
				basic,
				cameraCompatibility,
				featureAdoption,
				consentedDisclosureRevision
			},
			disclosure: {
				currentDisclosureRevision,
				schemaVersion: '1',
				receiverOrigin,
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
				renewalRequired:
					basic &&
					consentedDisclosureRevision > 0 &&
					consentedDisclosureRevision < currentDisclosureRevision,
				pendingRenewalFields:
					basic && consentedDisclosureRevision < currentDisclosureRevision
						? [`disclosure_revision_${currentDisclosureRevision}`]
						: []
			},
			status: {
				state:
					pendingDeletionCount > 0
						? UsageReportingLifecycleState.DELETION_PENDING
						: basic
							? UsageReportingLifecycleState.SCHEDULED
							: UsageReportingLifecycleState.DISABLED,
				lastFailure: UsageReportingFailureKind.NONE,
				pendingDeletionCount,
				identityConfigured,
				lastDeletionFailure
			}
		});

	return {
		actions,
		setOfflineDeletes(offline: boolean) {
			offlineDeletes = offline;
		},
		handler(command: UsageReportingCommand) {
			const action = command.action;
			if (!action.case) {
				throw new Error('missing action');
			}
			actions.push(action.case);
			if (action.case === 'get') {
				return create(UsageReportingResultSchema, {
					result: { case: 'state', value: currentState() }
				});
			}
			if (action.case === 'preview') {
				const payload: Record<string, unknown> = {
					schema_version: 1,
					disclosure_revision: currentDisclosureRevision,
					report_sequence: 1,
					installation_id: identityConfigured
						? '550e8400-e29b-41d4-a716-446655440000'
						: '00000000-0000-4000-8000-000000000000',
					basic: { keeppeek_version: '0.1.0-alpha.1', os_family: 'linux' }
				};
				if (action.value.cameraCompatibility) {
					payload.camera_compatibility = { configured_cameras: 1, groups: [] };
				}
				if (action.value.featureAdoption) {
					payload.feature_adoption = { recording_modes: [] };
				}
				const payloadJson = JSON.stringify(payload);
				return create(UsageReportingResultSchema, {
					result: {
						case: 'preview',
						value: create(UsageReportingPreviewSchema, {
							payloadJson,
							payloadBytes: payloadJson.length,
							maxPayloadBytes: 65_536,
							usesPlaceholderIdentity: !identityConfigured,
							valid: true,
							validationError: UsageReportingFailureKind.NONE
						})
					}
				});
			}
			if (action.case === 'update') {
				if (action.value.expectedConfigurationRevision !== `rev-${revision}`) {
					throw new Error('configuration revision conflicts');
				}
				if (
					!action.value.basic &&
					(action.value.cameraCompatibility || action.value.featureAdoption)
				) {
					throw new Error('basic system data is required');
				}
				revision += 1;
				const wasEnabled = basic;
				basic = action.value.basic;
				cameraCompatibility = action.value.cameraCompatibility;
				featureAdoption = action.value.featureAdoption;
				if (basic) {
					consentedDisclosureRevision =
						action.value.acceptedDisclosureRevision || consentedDisclosureRevision;
					identityConfigured = true;
				} else if (wasEnabled) {
					identityConfigured = false;
					if (offlineDeletes) {
						pendingDeletionCount += 1;
						lastDeletionFailure = UsageReportingFailureKind.NETWORK_UNAVAILABLE;
					} else {
						pendingDeletionCount = 0;
						lastDeletionFailure = UsageReportingFailureKind.NONE;
					}
				}
				return create(UsageReportingResultSchema, {
					result: { case: 'state', value: currentState() }
				});
			}
			if (action.case === 'resetIdentity') {
				if (action.value.expectedConfigurationRevision !== `rev-${revision}`) {
					throw new Error('configuration revision conflicts');
				}
				if (offlineDeletes) {
					pendingDeletionCount += 1;
					lastDeletionFailure = UsageReportingFailureKind.NETWORK_UNAVAILABLE;
				}
				return create(UsageReportingResultSchema, {
					result: { case: 'state', value: currentState() }
				});
			}
			if (action.case === 'deleteData') {
				if (action.value.expectedConfigurationRevision !== `rev-${revision}`) {
					throw new Error('configuration revision conflicts');
				}
				if (!offlineDeletes) {
					pendingDeletionCount = 0;
					lastDeletionFailure = UsageReportingFailureKind.NONE;
				}
				return create(UsageReportingResultSchema, {
					result: { case: 'state', value: currentState() }
				});
			}
			throw new Error('unsupported action');
		}
	};
}

test('default disabled install and non-administrator sessions make zero receiver requests and show no onboarding prompt', async ({
	page
}) => {
	const receiverRequests: string[] = [];
	page.on('request', (request) => {
		if (isReceiverRequest(request.url())) {
			receiverRequests.push(request.url());
		}
	});

	const harness = createUsageHarness();
	await mockControlPeer(page, {
		runtimeConfiguration,
		accessRole: 'user',
		accessLocal: false,
		capabilityIds: [usageReportingCapability],
		onUsageReportingCommand: (command) => harness.handler(command)
	});

	// Home and Cameras routes never show an onboarding telemetry prompt.
	await page.goto('/');
	await expect(page.getByText(/Usage reporting/i)).toHaveCount(0);

	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto('/settings');
	await expect(page).toHaveURL(/\/$/);
	await expect(page.getByRole('heading', { name: 'Dashboard', level: 1 })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Usage reporting' })).toHaveCount(0);
	expect(receiverRequests).toEqual([]);
	expect(harness.actions).toEqual([]);
});

test('administrator can review disclosure, preview JSON, opt in, reset ID, disable offline, and retry deletion', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const receiverRequests: string[] = [];
	page.on('request', (request) => {
		if (isReceiverRequest(request.url())) {
			receiverRequests.push(request.url());
		}
	});

	const harness = createUsageHarness({ offlineDeletes: true });
	await mockControlPeer(page, {
		runtimeConfiguration,
		accessRole: 'administrator',
		accessLocal: true,
		capabilityIds: [usageReportingCapability],
		onUsageReportingCommand: (command) => harness.handler(command)
	});

	await page.goto('/settings#usage-reporting');
	await expect(page.getByRole('heading', { name: 'Usage reporting' })).toBeVisible();
	await expect(page.getByText(/pseudonymous/i)).toBeVisible();
	await expect(page.getByText(/60 days/i)).toBeVisible();

	const basic = page.getByLabel(/Basic system data/i);
	const camera = page.getByLabel(/Camera compatibility data/i);
	const feature = page.getByLabel(/Feature adoption data/i);

	await expect(basic).not.toBeChecked();
	await expect(camera).toBeDisabled();
	await expect(feature).toBeDisabled();

	await basic.check();
	await camera.check();
	await feature.check();

	await page.getByRole('button', { name: /Preview exact JSON payload/i }).click();
	const previewBox = page.getByTestId('usage-reporting-preview-json');
	await expect(previewBox).toBeVisible();
	await expect(previewBox).toContainText('"camera_compatibility"');
	await expect(previewBox).toContainText('"feature_adoption"');

	await page.getByRole('button', { name: /Save usage reporting preferences/i }).click();
	await expect(page.getByTestId('usage-reporting-lifecycle')).toHaveText('Scheduled');

	// Reset while receiver is offline records a truthful pending deletion.
	await page.getByRole('button', { name: /Reset installation ID/i }).click();
	await expect(page.getByTestId('usage-reporting-pending-deletions')).toHaveText('1');
	await expect(page.getByText(/Last deletion issue: Network unavailable/i)).toBeVisible();

	// Bring receiver back online and retry deletion explicitly.
	harness.setOfflineDeletes(false);
	await page.getByRole('button', { name: /Retry deletion/i }).click();
	await expect(page.getByTestId('usage-reporting-pending-deletions')).toHaveText('0');

	// Turning Basic off disables all categories immediately.
	await basic.uncheck();
	await expect(camera).not.toBeChecked();
	await expect(feature).not.toBeChecked();
	await page.getByRole('button', { name: /Save usage reporting preferences/i }).click();
	await expect(page.getByTestId('usage-reporting-lifecycle')).toHaveText('Disabled');
	expect(receiverRequests).toEqual([]);
});

test('renewed consent workflow keeps new fields gated until accepted and works on mobile viewport', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const harness = createUsageHarness({
		initialBasic: true,
		consentedRevision: 1,
		currentRevision: 2
	});
	await mockControlPeer(page, {
		runtimeConfiguration,
		accessRole: 'administrator',
		accessLocal: true,
		capabilityIds: [usageReportingCapability],
		onUsageReportingCommand: (command) => harness.handler(command)
	});

	await page.goto('/settings#usage-reporting');
	await expect(page.getByRole('heading', { name: 'Usage reporting' })).toBeVisible();
	await expect(page.getByText(/Updated usage reporting disclosure \(revision 2\)/i)).toBeVisible();

	// Saving a category change during pending renewal preserves prior consent revision.
	await page.getByLabel(/Camera compatibility data/i).check();
	await page.getByRole('button', { name: /Save usage reporting preferences/i }).click();
	await expect(page.getByText(/Updated usage reporting disclosure \(revision 2\)/i)).toBeVisible();

	await page.getByRole('button', { name: /Keep existing consent only/i }).click();
	await expect(page.getByText(/Updated usage reporting disclosure \(revision 2\)/i)).toBeVisible();

	await page.getByRole('button', { name: /Accept updated disclosure/i }).click();
	await expect(page.getByText(/Updated usage reporting disclosure \(revision 2\)/i)).toHaveCount(0);
});
