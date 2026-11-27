import { create } from '@bufbuild/protobuf';
import { describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { render } from 'vitest-browser-svelte';
import McpAccessPanel from './McpAccessPanel.svelte';
import { McpAccessStateSchema, type McpAccessState } from '$lib/proto/webrtc_pb';

function makeState(overrides?: {
	enabled?: boolean;
	enabledAtUnixSecs?: bigint;
	enabledDurationSecs?: bigint;
	longEnabledWarningActive?: boolean;
	longEnabledWarningSilenced?: boolean;
	humanControlAllowed?: boolean;
	revision?: string;
}): McpAccessState {
	return create(McpAccessStateSchema, {
		configurationRevision: overrides?.revision ?? 'rev-1',
		enabled: overrides?.enabled ?? false,
		allowWrites: false,
		allowSnapshots: false,
		allowCameraDeviceWrites: false,
		enabledAtUnixSecs: overrides?.enabledAtUnixSecs,
		enabledDurationSecs: overrides?.enabledDurationSecs,
		longEnabledWarningThresholdSecs: 604_800n,
		longEnabledWarningActive: overrides?.longEnabledWarningActive ?? false,
		longEnabledWarningSilenced: overrides?.longEnabledWarningSilenced ?? false,
		humanControlAllowed: overrides?.humanControlAllowed ?? true
	});
}

describe('McpAccessPanel', () => {
	it('requires explicit human confirmation to enable MCP and allows immediate live disable', async () => {
		const onstatechange = vi.fn();
		const controller = {
			getState: vi.fn(async () => makeState({ enabled: false })),
			setEnabled: vi.fn(async (enabled: boolean) =>
				makeState({
					enabled,
					enabledAtUnixSecs: enabled ? 1_750_000_000n : undefined,
					enabledDurationSecs: enabled ? 10n : undefined,
					revision: enabled ? 'rev-2' : 'rev-3'
				})
			),
			silenceLongEnabledWarning: vi.fn(async () => makeState())
		};

		render(McpAccessPanel, { controller, onstatechange });

		await expect.element(page.getByTestId('mcp-access-status')).toHaveTextContent('DISABLED');
		await page.getByRole('button', { name: /Enable MCP access/i }).click();

		const confirmButton = page.getByRole('button', { name: /Confirm enable MCP/i });
		await expect.element(confirmButton).toBeDisabled();
		await page.getByLabelText(/I confirm I want to enable MCP access/i).click();
		await expect.element(confirmButton).not.toBeDisabled();
		await confirmButton.click();

		expect(controller.setEnabled).toHaveBeenCalledWith(true, true, 'rev-1');
		await expect.element(page.getByTestId('mcp-access-status')).toHaveTextContent('ENABLED');
		expect(onstatechange).toHaveBeenCalledTimes(1);

		await page.getByRole('button', { name: /^Disable MCP$/i }).click();
		expect(controller.setEnabled).toHaveBeenCalledWith(false, false, 'rev-2');
		await expect.element(page.getByTestId('mcp-access-status')).toHaveTextContent('DISABLED');
	});

	it('renders the 7-day warning, supports human silencing, and gates Bearer sessions', async () => {
		const controller = {
			getState: vi.fn(async () =>
				makeState({
					enabled: true,
					enabledAtUnixSecs: 1_700_000_000n,
					enabledDurationSecs: 9n * 86_400n,
					longEnabledWarningActive: true,
					humanControlAllowed: true
				})
			),
			setEnabled: vi.fn(async () => makeState({ enabled: false, revision: 'rev-3' })),
			silenceLongEnabledWarning: vi.fn(async () =>
				makeState({
					enabled: true,
					enabledAtUnixSecs: 1_700_000_000n,
					enabledDurationSecs: 9n * 86_400n,
					longEnabledWarningActive: false,
					longEnabledWarningSilenced: true,
					humanControlAllowed: true,
					revision: 'rev-2'
				})
			)
		};

		const view = await render(McpAccessPanel, { controller });

		await expect.element(page.getByTestId('mcp-panel-long-enabled-warning')).toBeVisible();
		await expect.element(page.getByTestId('mcp-enabled-duration')).toHaveTextContent('9 days');

		await page.getByRole('button', { name: /Silence warning/i }).click();
		expect(controller.silenceLongEnabledWarning).toHaveBeenCalledWith('rev-1');
		await expect.element(page.getByTestId('mcp-warning-silenced-note')).toBeVisible();

		await view.unmount();

		// Bearer sessions cannot enable MCP or silence warnings.
		const bearerController = {
			getState: vi.fn(async () =>
				makeState({
					enabled: false,
					humanControlAllowed: false
				})
			),
			setEnabled: vi.fn(async () => makeState()),
			silenceLongEnabledWarning: vi.fn(async () => makeState())
		};
		render(McpAccessPanel, { controller: bearerController });
		await expect.element(page.getByTestId('mcp-human-control-required')).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Enable MCP access/i })).toBeDisabled();
	});
});
