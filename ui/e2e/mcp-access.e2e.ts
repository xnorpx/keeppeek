import { create } from '@bufbuild/protobuf';
import { expect, test } from '@playwright/test';
import { mockControlPeer, type HealthFixture } from './fixtures/control-peer';
import { mcpAccessCapability } from '../src/lib/control-client-mcp-access';
import {
	McpAccessResultSchema,
	McpAccessStateSchema,
	type McpAccessCommand
} from '../src/lib/proto/webrtc_pb';

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

const mcpWarningIssue = {
	severity: 'warning' as const,
	scope: 'mcp',
	message:
		'MCP access has been enabled for 8 days. Disable MCP when no longer needed, or silence this warning in Settings.'
};

function createMcpHarness(options?: {
	initialEnabled?: boolean;
	initialDaysEnabled?: number;
	initialSilenced?: boolean;
	humanControlAllowed?: boolean;
}) {
	let revision = 1;
	let enabled = options?.initialEnabled ?? false;
	let enabledDays = options?.initialDaysEnabled ?? 0;
	let silenced = options?.initialSilenced ?? false;
	const humanControlAllowed = options?.humanControlAllowed ?? true;
	const actions: string[] = [];

	const currentState = () => {
		const durationSecs = enabled ? BigInt(enabledDays * 86_400) : undefined;
		const active = enabled && enabledDays >= 7 && !silenced;
		return create(McpAccessStateSchema, {
			configurationRevision: `rev-${revision}`,
			enabled,
			allowWrites: false,
			allowSnapshots: false,
			allowCameraDeviceWrites: false,
			enabledAtUnixSecs: enabled
				? BigInt(Math.floor(Date.now() / 1_000) - enabledDays * 86_400)
				: undefined,
			enabledDurationSecs: durationSecs,
			longEnabledWarningThresholdSecs: 604_800n,
			longEnabledWarningActive: active,
			longEnabledWarningSilenced: silenced,
			humanControlAllowed
		});
	};

	return {
		actions,
		handler(command: McpAccessCommand) {
			const action = command.action;
			if (!action.case) throw new Error('missing action');
			actions.push(action.case);
			if (action.case === 'get') {
				return create(McpAccessResultSchema, {
					result: { case: 'state', value: currentState() }
				});
			}
			if (action.case === 'setEnabled') {
				if (action.value.expectedConfigurationRevision !== `rev-${revision}`) {
					throw new Error('configuration revision conflicts; reload settings and retry');
				}
				if (action.value.enabled) {
					if (!humanControlAllowed) {
						throw new Error('enabling MCP access requires a human Administrator');
					}
					if (!action.value.confirm) {
						throw new Error('explicit confirmation is required to enable MCP access');
					}
					revision += 1;
					enabled = true;
					enabledDays = 0;
					silenced = false;
				} else {
					revision += 1;
					enabled = false;
					enabledDays = 0;
					silenced = false;
				}
				return create(McpAccessResultSchema, {
					result: { case: 'state', value: currentState() }
				});
			}
			if (action.case === 'silenceLongEnabledWarning') {
				if (action.value.expectedConfigurationRevision !== `rev-${revision}`) {
					throw new Error('configuration revision conflicts; reload settings and retry');
				}
				if (!humanControlAllowed) {
					throw new Error('silencing the MCP long-enabled warning requires a human Administrator');
				}
				if (!enabled) {
					throw new Error('MCP must be enabled before its long-enabled warning can be silenced');
				}
				revision += 1;
				silenced = true;
				return create(McpAccessResultSchema, {
					result: { case: 'state', value: currentState() }
				});
			}
			throw new Error('unsupported action');
		}
	};
}

test('administrator sees 7-day MCP warning banner, silences it, disables MCP, and re-enables with confirmation', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const harness = createMcpHarness({
		initialEnabled: true,
		initialDaysEnabled: 8,
		humanControlAllowed: true
	});
	const warningHealth: HealthFixture = {
		status: 'healthy',
		issues: [mcpWarningIssue]
	};
	const cleanHealth: HealthFixture = {
		status: 'healthy',
		issues: []
	};

	await mockControlPeer(page, {
		runtimeConfiguration,
		accessRole: 'administrator',
		accessLocal: true,
		capabilityIds: [mcpAccessCapability],
		healthSequence: [warningHealth, warningHealth, cleanHealth],
		onMcpAccessCommand: (command) => harness.handler(command)
	});

	await page.goto('/settings#integrations');
	await expect(page.getByRole('heading', { name: 'Settings', level: 1 })).toBeVisible({
		timeout: 20_000
	});
	const banner = page.getByTestId('mcp-long-enabled-banner');
	await expect(banner).toBeVisible({ timeout: 15_000 });
	await expect(banner).toContainText('MCP access has been enabled for 8 days');

	// Silence directly from the shell banner; panel updates live without a reload.
	await banner.getByRole('button', { name: /Silence warning/i }).click();
	await expect(banner).toHaveCount(0);

	const panel = page.locator('[data-mcp-access-panel]');
	await expect(panel).toBeVisible();
	await expect(panel.getByTestId('mcp-warning-silenced-note')).toBeVisible();

	await panel.getByRole('button', { name: /^Disable MCP$/i }).click();
	await expect(panel.getByTestId('mcp-access-status')).toHaveText('DISABLED');

	// Re-enabling requires explicit confirmation checkbox before Confirm enable MCP unlocks.
	await panel.getByRole('button', { name: /Enable MCP access/i }).click();
	const confirmButton = panel.getByRole('button', { name: /Confirm enable MCP/i });
	await expect(confirmButton).toBeDisabled();
	await panel.getByLabel(/I confirm I want to enable MCP access/i).check();
	await expect(confirmButton).toBeEnabled();
	await confirmButton.click();

	await expect(panel.getByTestId('mcp-access-status')).toHaveText('ENABLED');
	await expect(panel.getByTestId('mcp-warning-silenced-note')).toHaveCount(0);
	expect(harness.actions).toContain('silenceLongEnabledWarning');
	expect(harness.actions).toContain('setEnabled');
});

test('non-administrator user never sees the MCP 7-day warning banner or MCP panel', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const harness = createMcpHarness({
		initialEnabled: true,
		initialDaysEnabled: 10
	});

	await mockControlPeer(page, {
		runtimeConfiguration,
		accessRole: 'user',
		accessLocal: false,
		capabilityIds: [mcpAccessCapability],
		health: {
			status: 'healthy',
			issues: [mcpWarningIssue]
		},
		onMcpAccessCommand: (command) => harness.handler(command)
	});

	await page.goto('/');
	await expect(page.getByRole('heading', { name: 'Dashboard', level: 1 })).toBeVisible({
		timeout: 20_000
	});
	await expect(page.getByTestId('mcp-long-enabled-banner')).toHaveCount(0);

	await page.goto('/settings#integrations');
	await expect(page).toHaveURL(/\/$/, { timeout: 15_000 });
	await expect(page.locator('[data-mcp-access-panel]')).toHaveCount(0);
	expect(harness.actions).toEqual([]);
});

test('remote bearer administrator sees silence disabled on banner and panel while disable remains available', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const harness = createMcpHarness({
		initialEnabled: true,
		initialDaysEnabled: 9,
		humanControlAllowed: false
	});

	await mockControlPeer(page, {
		runtimeConfiguration,
		accessRole: 'administrator',
		accessLocal: false,
		capabilityIds: [mcpAccessCapability],
		health: {
			status: 'degraded',
			issues: [mcpWarningIssue]
		},
		onMcpAccessCommand: (command) => harness.handler(command)
	});

	await page.goto('/settings#integrations');
	await expect(page.getByRole('heading', { name: 'Settings', level: 1 })).toBeVisible({
		timeout: 20_000
	});
	const banner = page.getByTestId('mcp-long-enabled-banner');
	await expect(banner).toBeVisible({ timeout: 15_000 });
	await expect(banner.getByRole('button', { name: /Silence warning/i })).toBeDisabled();
	await expect(banner.getByRole('button', { name: /^Disable MCP$/i })).toBeEnabled();

	const panel = page.locator('[data-mcp-access-panel]');
	await expect(panel.getByTestId('mcp-human-control-required')).toBeVisible();
	await expect(panel.getByRole('button', { name: /Silence warning/i })).toBeDisabled();
	await banner.getByRole('button', { name: /^Disable MCP$/i }).click();
	await expect(panel.getByTestId('mcp-access-status')).toHaveText('DISABLED');
	await expect(panel.getByRole('button', { name: /Enable MCP access/i })).toBeDisabled();
});
