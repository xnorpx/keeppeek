import { create } from '@bufbuild/protobuf';
import {
	McpAccessCommandSchema,
	type McpAccessState,
	type Ok,
	type Request
} from './proto/webrtc_pb';

export const mcpAccessCapability = 'keeppeek.mcp-access.v1' as const;

type Send = (command: Request['command'], timeoutMs?: number) => Promise<Ok['result']>;
const invalidResponseMessage = 'Server returned an invalid MCP access response.';

export function formatMcpEnabledDuration(seconds: bigint | number | undefined): string {
	if (seconds === undefined) return 'Unknown duration';
	const total = typeof seconds === 'bigint' ? Number(seconds) : seconds;
	if (!Number.isFinite(total) || total < 0) return 'Unknown duration';
	const days = Math.floor(total / 86_400);
	const hours = Math.floor((total % 86_400) / 3_600);
	if (days >= 1) {
		const dayLabel = days === 1 ? '1 day' : `${days} days`;
		if (hours === 0) return dayLabel;
		return `${dayLabel} ${hours}h`;
	}
	if (hours >= 1) {
		return hours === 1 ? '1 hour' : `${hours} hours`;
	}
	const minutes = Math.floor(total / 60);
	if (minutes >= 1) {
		return minutes === 1 ? '1 minute' : `${minutes} minutes`;
	}
	return 'Less than a minute';
}

export class McpAccessControlClient {
	#stateListeners = new Set<(state: McpAccessState) => void>();

	constructor(private readonly send: Send) {}

	onStateChange(listener: (state: McpAccessState) => void): () => void {
		this.#stateListeners.add(listener);
		return () => this.#stateListeners.delete(listener);
	}

	async getState(): Promise<McpAccessState> {
		const result = await this.request({ case: 'get', value: {} });
		if (result.case !== 'state') throw new Error(invalidResponseMessage);
		return result.value;
	}

	async setEnabled(
		enabled: boolean,
		confirm: boolean,
		expectedConfigurationRevision: string
	): Promise<McpAccessState> {
		if (!expectedConfigurationRevision.trim()) {
			throw new Error('Reload MCP settings before changing access.');
		}
		if (enabled && !confirm) {
			throw new Error('Explicit confirmation is required before enabling MCP access.');
		}
		const result = await this.request({
			case: 'setEnabled',
			value: {
				expectedConfigurationRevision,
				enabled,
				confirm
			}
		});
		if (result.case !== 'state') throw new Error(invalidResponseMessage);
		this.#notify(result.value);
		return result.value;
	}

	async silenceLongEnabledWarning(expectedConfigurationRevision: string): Promise<McpAccessState> {
		if (!expectedConfigurationRevision.trim()) {
			throw new Error('Reload MCP settings before silencing the warning.');
		}
		const result = await this.request({
			case: 'silenceLongEnabledWarning',
			value: { expectedConfigurationRevision }
		});
		if (result.case !== 'state') throw new Error(invalidResponseMessage);
		this.#notify(result.value);
		return result.value;
	}

	#notify(state: McpAccessState): void {
		for (const listener of this.#stateListeners) {
			listener(state);
		}
	}

	private async request(
		action: NonNullable<Parameters<typeof create<typeof McpAccessCommandSchema>>[1]>['action']
	) {
		const response = await this.send({
			case: 'mcpAccessCommand',
			value: create(McpAccessCommandSchema, { action })
		});
		if (response?.case !== 'mcpAccessResult' || !response.value.result.case) {
			throw new Error(invalidResponseMessage);
		}
		return response.value.result;
	}
}
