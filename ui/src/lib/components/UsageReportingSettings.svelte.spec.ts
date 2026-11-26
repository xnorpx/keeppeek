import { create } from '@bufbuild/protobuf';
import { describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { render } from 'vitest-browser-svelte';
import UsageReportingSettings from './UsageReportingSettings.svelte';
import {
	UsageReportingFailureKind,
	UsageReportingLifecycleState,
	UsageReportingPreviewSchema,
	UsageReportingStateSchema,
	type UsageReportingState
} from '$lib/proto/webrtc_pb';

function makeState(overrides?: {
	basic?: boolean;
	cameraCompatibility?: boolean;
	featureAdoption?: boolean;
	consentedDisclosureRevision?: number;
	currentDisclosureRevision?: number;
	renewalRequired?: boolean;
	pendingRenewalFields?: string[];
	lifecycle?: UsageReportingLifecycleState;
	pendingDeletionCount?: number;
	identityConfigured?: boolean;
	lastDeletionFailure?: UsageReportingFailureKind;
	revision?: string;
}): UsageReportingState {
	return create(UsageReportingStateSchema, {
		configurationRevision: overrides?.revision ?? 'rev-1',
		consent: {
			basic: overrides?.basic ?? false,
			cameraCompatibility: overrides?.cameraCompatibility ?? false,
			featureAdoption: overrides?.featureAdoption ?? false,
			consentedDisclosureRevision: overrides?.consentedDisclosureRevision ?? 0
		},
		disclosure: {
			currentDisclosureRevision: overrides?.currentDisclosureRevision ?? 1,
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
			renewalRequired: overrides?.renewalRequired ?? false,
			pendingRenewalFields: overrides?.pendingRenewalFields ?? []
		},
		status: {
			state: overrides?.lifecycle ?? UsageReportingLifecycleState.DISABLED,
			lastFailure: UsageReportingFailureKind.NONE,
			pendingDeletionCount: overrides?.pendingDeletionCount ?? 0,
			identityConfigured: overrides?.identityConfigured ?? false,
			lastDeletionFailure: overrides?.lastDeletionFailure ?? UsageReportingFailureKind.NONE
		}
	});
}

describe('UsageReportingSettings', () => {
	it('renders informed consent, enforces Basic dependency, and previews exact JSON without saving', async () => {
		const controller = {
			getState: vi.fn(async () => makeState()),
			updateConsent: vi.fn(async () =>
				makeState({
					basic: true,
					cameraCompatibility: true,
					consentedDisclosureRevision: 1,
					lifecycle: UsageReportingLifecycleState.SCHEDULED,
					identityConfigured: true,
					revision: 'rev-2'
				})
			),
			preview: vi.fn(async () =>
				create(UsageReportingPreviewSchema, {
					payloadJson: '{"schema_version":1,"basic":{"os_family":"linux"}}',
					payloadBytes: 49,
					maxPayloadBytes: 65_536,
					usesPlaceholderIdentity: true,
					valid: true,
					validationError: UsageReportingFailureKind.NONE
				})
			),
			resetIdentity: vi.fn(async () => makeState()),
			deleteData: vi.fn(async () => makeState())
		};

		render(UsageReportingSettings, { controller });

		await expect.element(page.getByRole('heading', { name: 'Usage reporting' })).toBeVisible();
		await expect.element(page.getByText(/pseudonymous/i)).toBeVisible();
		await expect.element(page.getByText(/60 days/i)).toBeVisible();
		await expect.element(page.getByText(/10 installations/i)).toBeVisible();

		const basicBox = page.getByLabelText(/Basic system data/i);
		const cameraBox = page.getByLabelText(/Camera compatibility data/i);
		const featureBox = page.getByLabelText(/Feature adoption data/i);

		await expect.element(basicBox).not.toBeChecked();
		await expect.element(cameraBox).toBeDisabled();
		await expect.element(featureBox).toBeDisabled();

		await basicBox.click();
		await expect.element(cameraBox).not.toBeDisabled();
		await cameraBox.click();

		// Preview works before saving and never calls updateConsent.
		await page.getByRole('button', { name: /Preview exact JSON payload/i }).click();
		await expect.element(page.getByTestId('usage-reporting-preview-json')).toBeVisible();
		await expect.element(page.getByText(/49 \/ 65536 bytes/)).toBeVisible();
		expect(controller.updateConsent).not.toHaveBeenCalled();

		// Unchecking Basic cascades off Camera compatibility.
		await basicBox.click();
		await expect.element(cameraBox).not.toBeChecked();
		await expect.element(cameraBox).toBeDisabled();

		// Re-enable Basic + Camera and save.
		await basicBox.click();
		await cameraBox.click();
		await page.getByRole('button', { name: /Save usage reporting preferences/i }).click();
		expect(controller.updateConsent).toHaveBeenCalledWith(
			{
				basic: true,
				cameraCompatibility: true,
				featureAdoption: false,
				acceptedDisclosureRevision: 1
			},
			'rev-1'
		);
		await expect
			.element(page.getByTestId('usage-reporting-lifecycle'))
			.toHaveTextContent('Scheduled');
	});

	it('handles renewed consent accept/keep-existing, revision conflicts, and offline deletion retry', async () => {
		let state = makeState({
			basic: true,
			consentedDisclosureRevision: 1,
			currentDisclosureRevision: 2,
			renewalRequired: true,
			pendingRenewalFields: ['disclosure_revision_2'],
			lifecycle: UsageReportingLifecycleState.DELETION_PENDING,
			pendingDeletionCount: 1,
			identityConfigured: true,
			lastDeletionFailure: UsageReportingFailureKind.NETWORK_UNAVAILABLE
		});
		const controller = {
			getState: vi.fn(async () => state),
			updateConsent: vi.fn(async (input) => {
				state = makeState({
					basic: input.basic,
					cameraCompatibility: input.cameraCompatibility,
					featureAdoption: input.featureAdoption,
					consentedDisclosureRevision: input.acceptedDisclosureRevision,
					currentDisclosureRevision: 2,
					renewalRequired: input.acceptedDisclosureRevision < 2,
					pendingRenewalFields:
						input.acceptedDisclosureRevision < 2 ? ['disclosure_revision_2'] : [],
					lifecycle: UsageReportingLifecycleState.DELETION_PENDING,
					pendingDeletionCount: 1,
					identityConfigured: true,
					revision: 'rev-2'
				});
				return state;
			}),
			preview: vi.fn(async () =>
				create(UsageReportingPreviewSchema, {
					payloadJson: '{}',
					payloadBytes: 2,
					maxPayloadBytes: 65_536,
					usesPlaceholderIdentity: false,
					valid: true,
					validationError: UsageReportingFailureKind.NONE
				})
			),
			resetIdentity: vi.fn(async () => {
				throw new Error('configuration revision conflicts; reload settings and retry');
			}),
			deleteData: vi.fn(async () => {
				state = makeState({
					basic: true,
					consentedDisclosureRevision: 2,
					currentDisclosureRevision: 2,
					renewalRequired: false,
					lifecycle: UsageReportingLifecycleState.SCHEDULED,
					pendingDeletionCount: 0,
					identityConfigured: true,
					revision: 'rev-2'
				});
				return state;
			})
		};

		render(UsageReportingSettings, { controller });

		await expect
			.element(page.getByText(/Updated usage reporting disclosure \(revision 2\)/i))
			.toBeVisible();
		await expect.element(page.getByText(/Last deletion issue: Network unavailable/i)).toBeVisible();

		// Saving a category change during pending renewal preserves prior consent revision 1.
		await page.getByLabelText(/Camera compatibility data/i).click();
		await page.getByRole('button', { name: /Save usage reporting preferences/i }).click();
		expect(controller.updateConsent).toHaveBeenCalledWith(
			expect.objectContaining({ cameraCompatibility: true, acceptedDisclosureRevision: 1 }),
			'rev-1'
		);
		await expect
			.element(page.getByText(/Updated usage reporting disclosure \(revision 2\)/i))
			.toBeVisible();

		// Keep existing consent leaves renewal banner active and sends prior revision 1.
		await page.getByRole('button', { name: /Keep existing consent only/i }).click();
		expect(controller.updateConsent).toHaveBeenCalledWith(
			expect.objectContaining({ acceptedDisclosureRevision: 1 }),
			'rev-2'
		);

		// Accepting updated disclosure advances to revision 2.
		await page.getByRole('button', { name: /Accept updated disclosure/i }).click();
		expect(controller.updateConsent).toHaveBeenCalledWith(
			expect.objectContaining({ acceptedDisclosureRevision: 2 }),
			'rev-2'
		);

		// Retry deletion drains pending deletions cleanly.
		await page.getByRole('button', { name: /Retry deletion/i }).click();
		expect(controller.deleteData).toHaveBeenCalledWith('rev-2');
		await expect
			.element(page.getByTestId('usage-reporting-pending-deletions'))
			.toHaveTextContent('0');

		// Conflicting reset prompts Reload settings.
		await page.getByRole('button', { name: /Reset installation ID/i }).click();
		await expect.element(page.getByText(/configuration revision conflicts/i)).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Reload settings/i })).toBeVisible();
	});
});
