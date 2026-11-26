import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ControlClient } from '$lib/control-client';
import type {
	CloudNotificationSettings,
	CloudNotificationStatus,
	CloudNotificationTestResult
} from '$lib/control-client-cloud-notifications';
import CloudNotificationSettingsFixture from './CloudNotificationSettings.fixture.svelte';

function makeStatus(overrides: Partial<CloudNotificationStatus> = {}): CloudNotificationStatus {
	return {
		state: 'disabled',
		paired: true,
		pending: false,
		acceptedLastHour: 0,
		acceptedLastDay: 0,
		suppressedLastHour: 0,
		suppressedLastDay: 0,
		nextEligibleAtMs: null,
		lastAcceptedAtMs: null,
		lastFailure: 'none',
		...overrides
	};
}

describe('CloudNotificationSettings', () => {
	it('keeps controls unavailable when keeppeek.cloud-notifications.v1 is absent', async () => {
		const client = {
			getCloudNotificationSettings: vi.fn(),
			getCloudNotificationStatus: vi.fn()
		} as unknown as ControlClient;

		render(CloudNotificationSettingsFixture, { client, capabilities: [] });
		await expect.element(page.getByTestId('cloud-push-unavailable')).toBeVisible();
		expect(client.getCloudNotificationSettings).not.toHaveBeenCalled();
	});

	it('renders unpaired, throttled, backing_off, unavailable states, preview gating, and test limits', async () => {
		let currentSettings: CloudNotificationSettings = {
			revision: 1n,
			enabled: false,
			imagePreviewsEnabled: false
		};
		let currentStatus: CloudNotificationStatus = makeStatus({
			state: 'unpaired',
			paired: false
		});
		const getCloudNotificationSettings = vi.fn(async () => currentSettings);
		const getCloudNotificationStatus = vi.fn(async () => currentStatus);
		const updateCloudNotificationSettings = vi.fn(
			async (expectedRevision: bigint, enabled: boolean, imagePreviewsEnabled: boolean) => {
				if (expectedRevision !== currentSettings.revision) {
					throw new Error('cloud notification settings revision conflict');
				}
				currentSettings = {
					revision: currentSettings.revision + 1n,
					enabled,
					imagePreviewsEnabled
				};
				currentStatus = makeStatus({
					state: enabled ? 'ready' : 'disabled',
					paired: true
				});
				return currentSettings;
			}
		);
		const testCloudNotification = vi.fn(async (): Promise<CloudNotificationTestResult> => ({
			accepted: false,
			nextEligibleAtMs: Date.UTC(2026, 8, 19, 13, 0, 0),
			failure: 'local_limit'
		}));
		const client = {
			getCloudNotificationSettings,
			getCloudNotificationStatus,
			updateCloudNotificationSettings,
			testCloudNotification
		} as unknown as ControlClient;

		render(CloudNotificationSettingsFixture, { client });
		await expect.element(page.getByTestId('cloud-push-state')).toHaveTextContent('UNPAIRED');

		// Image preview checkbox stays disabled while unpaired and disabled.
		const previewCheckbox = page.getByLabelText(/Allow private direct still-image previews/i);
		await expect.element(previewCheckbox).toBeDisabled();

		// Transition to paired + ready and verify enabling signaling unlocks preview opt-in.
		currentStatus = makeStatus({
			state: 'throttled',
			paired: true,
			pending: true,
			acceptedLastHour: 120,
			acceptedLastDay: 240,
			suppressedLastHour: 9,
			suppressedLastDay: 42,
			lastFailure: 'service_limit'
		});
		await userEvent.click(page.getByRole('button', { name: 'Refresh cloud status' }));
		await expect.element(page.getByTestId('cloud-push-state')).toHaveTextContent('THROTTLED');
		await expect.element(page.getByText('120 / 240')).toBeVisible();
		await expect.element(page.getByText('9 / 42')).toBeVisible();

		const enableCheckbox = page.getByLabelText(/Enable Keeppeek Service signaling/i);
		await userEvent.click(enableCheckbox);
		await expect.element(previewCheckbox).toBeEnabled();
		await userEvent.click(previewCheckbox);
		await userEvent.click(page.getByRole('button', { name: 'Save cloud settings' }));

		expect(updateCloudNotificationSettings).toHaveBeenCalledWith(1n, true, true);
		await expect.element(page.getByTestId('cloud-push-state')).toHaveTextContent('READY');

		// Test send reports local 3/hr limit truthfully without claiming phone display.
		currentStatus = makeStatus({
			state: 'backing_off',
			paired: true,
			pending: true,
			lastFailure: 'local_limit',
			nextEligibleAtMs: Date.UTC(2026, 8, 19, 13, 0, 0)
		});
		await userEvent.click(page.getByRole('button', { name: /Send test signal/i }));
		await expect.element(page.getByTestId('cloud-push-state')).toHaveTextContent('BACKING OFF');
		await expect.element(page.getByText(/Last test: Local pacing limit/)).toBeVisible();

		// Revision conflict reloads authoritative server state.
		currentSettings = { revision: 9n, enabled: true, imagePreviewsEnabled: false };
		currentStatus = makeStatus({
			state: 'unavailable',
			paired: true,
			lastFailure: 'credential_rejected'
		});
		await userEvent.click(previewCheckbox);
		await userEvent.click(page.getByRole('button', { name: 'Save cloud settings' }));
		await expect.element(page.getByTestId('cloud-push-state')).toHaveTextContent('UNAVAILABLE');
	});
});
