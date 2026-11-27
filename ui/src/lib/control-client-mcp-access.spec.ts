import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import { formatMcpEnabledDuration, McpAccessControlClient } from './control-client-mcp-access';
import {
	McpAccessResultSchema,
	McpAccessStateSchema,
	type Ok,
	type Request
} from './proto/webrtc_pb';

function sampleState(overrides?: Partial<{ enabled: boolean; silenced: boolean }>) {
	return create(McpAccessStateSchema, {
		configurationRevision: 'rev-1',
		enabled: overrides?.enabled ?? false,
		allowWrites: false,
		allowSnapshots: false,
		allowCameraDeviceWrites: false,
		longEnabledWarningThresholdSecs: 604_800n,
		longEnabledWarningActive: false,
		longEnabledWarningSilenced: overrides?.silenced ?? false,
		humanControlAllowed: true
	});
}

describe('McpAccessControlClient', () => {
	it('sends typed get, setEnabled, and silenceLongEnabledWarning commands', async () => {
		const seen: Array<{ action: string; enabled?: boolean; confirm?: boolean }> = [];
		const client = new McpAccessControlClient(async (command: Request['command']) => {
			if (command.case !== 'mcpAccessCommand') {
				throw new Error('unexpected command');
			}
			const action = command.value.action;
			if (action.case === 'setEnabled') {
				seen.push({
					action: action.case,
					enabled: action.value.enabled,
					confirm: action.value.confirm
				});
			} else {
				seen.push({ action: action.case ?? 'none' });
			}
			return {
				case: 'mcpAccessResult',
				value: create(McpAccessResultSchema, {
					result: { case: 'state', value: sampleState({ enabled: true }) }
				})
			} satisfies Ok['result'];
		});

		const loaded = await client.getState();
		expect(loaded.configurationRevision).toBe('rev-1');

		await client.setEnabled(true, true, 'rev-1');
		await client.silenceLongEnabledWarning('rev-1');
		await client.setEnabled(false, false, 'rev-1');

		expect(seen).toEqual([
			{ action: 'get' },
			{ action: 'setEnabled', enabled: true, confirm: true },
			{ action: 'silenceLongEnabledWarning' },
			{ action: 'setEnabled', enabled: false, confirm: false }
		]);
	});

	it('rejects unconfirmed enable and missing revisions before sending', async () => {
		let calls = 0;
		const client = new McpAccessControlClient(async () => {
			calls += 1;
			return { case: undefined, value: undefined };
		});

		await expect(client.setEnabled(true, false, 'rev-1')).rejects.toThrow(
			'Explicit confirmation is required'
		);
		await expect(client.setEnabled(false, false, '')).rejects.toThrow('Reload MCP settings');
		await expect(client.silenceLongEnabledWarning('   ')).rejects.toThrow('Reload MCP settings');
		expect(calls).toBe(0);
	});

	it('formats enabled duration cleanly across ranges', () => {
		expect(formatMcpEnabledDuration(undefined)).toBe('Unknown duration');
		expect(formatMcpEnabledDuration(30n)).toBe('Less than a minute');
		expect(formatMcpEnabledDuration(120n)).toBe('2 minutes');
		expect(formatMcpEnabledDuration(3_600n)).toBe('1 hour');
		expect(formatMcpEnabledDuration(86_400n)).toBe('1 day');
		expect(formatMcpEnabledDuration(8n * 86_400n + 3n * 3_600n)).toBe('8 days 3h');
	});
});
